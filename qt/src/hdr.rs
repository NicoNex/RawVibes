//! The HDR preview for a Qt HDR surface: the core renders it in GTK's rec2100-linear
//! (linear Rec.2020, 1 = SDR white); Qt's HDR swapchain takes extended linear sRGB (scRGB).

use std::sync::atomic::{AtomicU32, Ordering};

use half::f16;

/// Rec.2020 → Rec.709 (BT.2087), the inverse of grade.wgsl's REC709_TO_2020; out-of-709 colours go negative, as scRGB allows.
const REC2020_TO_709: [[f32; 3]; 3] =
    [[1.660491, -0.587641, -0.072850], [-0.124550, 1.132900, -0.008349], [-0.018151, -0.100579, 1.118730]];

/// What 1.0 (SDR white) becomes on the HDR swapchain, as it reports (hdrwindow.h): until it does,
/// scene-referred scRGB with SDR white at BT.2408's 203 nits (1.0 = 80 nits), or 1 on macOS EDR.
static WHITE: AtomicU32 = AtomicU32::new(if cfg!(target_os = "macos") { 1.0f32 } else { 203.0 / 80.0 }.to_bits());

pub fn white() -> f32 {
    f32::from_bits(WHITE.load(Ordering::Relaxed))
}

pub fn set_white(w: f32) {
    if w.is_finite() && w > 0.0 {
        WHITE.store(w.to_bits(), Ordering::Relaxed);
    }
}

/// `rgba16f` (rec2100-linear, half floats) converted in place to scRGB scaled by `white`.
pub fn to_scrgb(rgba16f: &mut [u8], white: f32) {
    for px in rgba16f.chunks_exact_mut(8) {
        let c = |i: usize| f16::from_le_bytes([px[2 * i], px[2 * i + 1]]).to_f32();
        let rgb = [c(0), c(1), c(2)];
        for (i, row) in REC2020_TO_709.iter().enumerate() {
            let v = (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]) * white;
            px[2 * i..2 * i + 2].copy_from_slice(&f16::from_f32(v).to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// grade.wgsl's REC709_TO_2020, row-major.
    const REC709_TO_2020: [[f32; 3]; 3] =
        [[0.627404, 0.329283, 0.043313], [0.069097, 0.919540, 0.011362], [0.016391, 0.088013, 0.895595]];

    #[test]
    fn the_matrices_are_inverses() {
        for i in 0..3 {
            for j in 0..3 {
                let v: f32 = (0..3).map(|k| REC2020_TO_709[i][k] * REC709_TO_2020[k][j]).sum();
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 1e-4, "{i},{j}: {v}");
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
