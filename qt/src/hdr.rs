//! The HDR preview for a Qt HDR surface: the core renders it in GTK's rec2100-linear
//! (linear Rec.2020, 1 = SDR white); Qt's HDR swapchain takes extended linear sRGB (scRGB).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use half::f16;
use rawvibes_core::profiles::rec2020_to_rec709;

/// What 1.0 (SDR white) becomes on the HDR swapchain, as it reports it (hdrwindow.h), and whether
/// that swapchain is HDR at all: the rendition is converted only then.
static WHITE: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0
static SURFACE: AtomicBool = AtomicBool::new(false);

pub fn surface() -> bool {
    SURFACE.load(Ordering::Acquire)
}

pub fn white() -> f32 {
    f32::from_bits(WHITE.load(Ordering::Relaxed))
}

/// What the HDR window's swapchain turned out to be. The scale is stored first: once the
/// surface reads HDR, its white is already there.
pub fn set_surface(hdr: bool, white: f32) {
    if white.is_finite() && white > 0.0 {
        WHITE.store(white.to_bits(), Ordering::Relaxed);
    }
    SURFACE.store(hdr, Ordering::Release);
}

/// `rgba16f` (rec2100-linear, half floats) converted in place to scRGB, SDR white at `white`;
/// out-of-709 colours go negative, as scRGB allows.
pub fn to_scrgb(rgba16f: &mut [u8], white: f32) {
    use half::slice::HalfFloatSliceExt;
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
