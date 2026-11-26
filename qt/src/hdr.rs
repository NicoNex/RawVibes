//! The HDR preview for a Qt HDR surface: the core renders it in GTK's rec2100-linear
//! (linear Rec.2020, 1 = SDR white). Qt's HDR swapchain takes HDR10 (Rec.2020 PQ: Linux,
//! Windows) or extended linear sRGB (scRGB: macOS EDR), as hdrwindow.h asked for.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use half::f16;
use half::slice::HalfFloatSliceExt;
use rawvibes_core::export::pq_encode;
use rawvibes_core::profiles::rec2020_to_rec709;
use rayon::prelude::*;

/// SDR white on the HDR swapchain, as it reports it (hdrwindow.h): cd/m² on an HDR10 one,
/// a multiplier on a scRGB one. Whether that swapchain is HDR at all (the rendition is
/// converted only then) and whether it is HDR10.
static WHITE: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0
static SURFACE: AtomicBool = AtomicBool::new(false);
static PQ: AtomicBool = AtomicBool::new(false);

pub fn surface() -> bool {
    SURFACE.load(Ordering::Acquire)
}

pub fn white() -> f32 {
    f32::from_bits(WHITE.load(Ordering::Relaxed))
}

/// What the HDR window's swapchain turned out to be. The scale is stored first: once the
/// surface reads HDR, its white is already there.
pub fn set_surface(hdr: bool, white: f32, pq: bool) {
    if white.is_finite() && white > 0.0 {
        WHITE.store(white.to_bits(), Ordering::Relaxed);
    }
    PQ.store(pq, Ordering::Relaxed);
    SURFACE.store(hdr, Ordering::Release);
}

/// `rgba16f` (rec2100-linear) converted in place for the surface: HDR10 or scRGB.
pub fn convert(rgba16f: &mut [u8]) {
    if PQ.load(Ordering::Relaxed) { to_pq(rgba16f, white()) } else { to_scrgb(rgba16f, white()) }
}

/// `rgba16f` (rec2100-linear, half floats) converted in place to Rec.2100 PQ code values,
/// SDR white at `white` cd/m²; the swapchain shows them as they are.
pub fn to_pq(rgba16f: &mut [u8], white: f32) {
    rgba16f.par_chunks_mut(8 * 1024).for_each(|chunk| {
        for px in chunk.as_chunks_mut::<8>().0 {
            for c in px.as_chunks_mut::<2>().0.iter_mut().take(3) {
                let v = f16::from_le_bytes(*c).to_f32().max(0.0);
                *c = f16::from_f32(pq_encode(v * white)).to_le_bytes();
            }
        }
    });
}

/// `rgba16f` (rec2100-linear, half floats) converted in place to scRGB, SDR white at `white`;
/// out-of-709 colours go negative, as scRGB allows.
pub fn to_scrgb(rgba16f: &mut [u8], white: f32) {
    let m = rec2020_to_rec709().map(|row| row.map(|v| v * white));
    // Bit copies, then half's batched conversions (SIMD where the CPU has it), not one value at a time.
    let mut halves: Vec<f16> = rgba16f.as_chunks::<2>().0.iter().map(|&b| f16::from_le_bytes(b)).collect();
    let mut f = vec![0f32; halves.len()];
    halves.convert_to_f32_slice(&mut f);
    for px in f.as_chunks_mut::<4>().0 {
        let rgb = [px[0], px[1], px[2]];
        for (i, row) in m.iter().enumerate() {
            px[i] = row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2];
        }
    }
    halves.convert_from_f32_slice(&f);
    for (out, h) in rgba16f.as_chunks_mut::<2>().0.iter_mut().zip(halves) {
        *out = h.to_le_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// grade.wgsl's REC709_TO_2020, which made the rendition: the conversion must undo it.
    const SHADER_709_TO_2020: [[f32; 3]; 3] =
        [[0.627404, 0.329283, 0.043313], [0.069097, 0.919540, 0.011362], [0.016391, 0.088013, 0.895595]];

    #[test]
    fn the_conversion_undoes_the_shaders_matrix() {
        let m = rec2020_to_rec709();
        for (i, row) in m.iter().enumerate() {
            for j in 0..3 {
                let v: f32 = row.iter().zip(&SHADER_709_TO_2020).map(|(a, col)| a * col[j]).sum();
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 1e-3, "{i},{j}: {v}");
            }
        }
    }

    #[test]
    fn hdr10_puts_sdr_white_at_its_nits() {
        let mut px: Vec<u8> = [1.0f32, 0.0, 4.0, 1.0].iter().flat_map(|&v| f16::from_f32(v).to_le_bytes()).collect();
        to_pq(&mut px, 203.0);
        let got = |i: usize| f16::from_le_bytes([px[2 * i], px[2 * i + 1]]).to_f32();
        // ST 2084: 203 cd/m² ≈ 0.5807, 812 ≈ 0.7255; black stays 0, alpha untouched.
        assert!((got(0) - 0.5807).abs() < 2e-3, "{}", got(0));
        assert_eq!(got(1), 0.0);
        assert!((got(2) - pq_encode(812.0)).abs() < 2e-3, "{}", got(2));
        assert_eq!(got(3), 1.0);
    }

    #[test]
    fn sdr_white_lands_on_white_times_the_scale() {
        let mut px: Vec<u8> = [1.0f32, 1.0, 1.0, 1.0].iter().flat_map(|&v| f16::from_f32(v).to_le_bytes()).collect();
        to_scrgb(&mut px, 2.0);
        for i in 0..3 {
            let v = f16::from_le_bytes([px[2 * i], px[2 * i + 1]]).to_f32();
            assert!((v - 2.0).abs() < 0.01, "{v}");
        }
        assert_eq!(f16::from_le_bytes([px[6], px[7]]).to_f32(), 1.0, "alpha untouched");
    }
}
