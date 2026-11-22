//! Edge-aware shadows and highlights: darktable's tone equalizer (Aurélien Pierre,
//! src/iop/toneequal.c) with its exposure-independent guided filter mask (EIGF,
//! src/common/eigf.h), in `tones.wgsl`.
//!
//! The mask is the pixel's RGB norm smoothed by the EIGF, so a gain follows areas
//! and stops at their edges. Gains are set on nine 1-EV nodes from −8 to 0 EV of the
//! mask and interpolated by eight Gaussian radial basis functions (σ √2, centres
//! 8/7 EV apart), fitted by least squares as darktable does, but on the gains in EV
//! (darktable fits linear factors): zero sliders give weights of exactly 0, so the
//! curve leaves the slider's first step continuous with off. The two sliders shape
//! the nodes like darktable's "compress shadows/highlights" presets: shadows move
//! −8…−5 EV, highlights −3…0 EV, in the ratio 1 : 5/3 : 5/3 : 1.

/// Full-resolution radius of the mask's guided filter: darktable's "medium" preset
/// (3 % diameter) on a 6000 px frame.
// shortcut: fixed in pixels, so relatively smaller on larger sensors; pass the frame
// size to `Detail::support` to make it relative when that matters.
pub(crate) const RADIUS: f32 = 90.0;
/// Guided filter iterations and feathering (regularisation 1/7), "medium" preset.
pub(crate) const ITERATIONS: usize = 3;
pub(crate) const FEATHER: f32 = 1.0 / 7.0;
/// Mask exposure compensation that puts scene grey at −4 EV, the middle node.
pub(crate) const MASK_BOOST: f32 = -1.57;
/// RBF width (EV).
const SIGMA: f64 = std::f64::consts::SQRT_2;
/// Peak EV of a slider at ±100.
const STEP: f32 = 1.0;

/// The eight RBF weights (EV) for `shadows` and `highlights` (−100…100).
pub(crate) fn factors(shadows: f32, highlights: f32) -> [f32; 8] {
    let (s, h) = (shadows / 100.0 * STEP, highlights / 100.0 * STEP);
    let shape = [1.0, 5.0 / 3.0, 5.0 / 3.0, 1.0];
    let mut ev = [0.0f32; 9];
    for k in 0..4 {
        ev[k] = s * shape[k];
        ev[5 + k] = h * shape[k];
    }
    solve(ev.map(f64::from))
}

fn centre(j: usize) -> f64 {
    -8.0 + 8.0 * j as f64 / 7.0
}

fn rbf(x: f64, j: usize) -> f64 {
    (-(x - centre(j)).powi(2) / (2.0 * SIGMA * SIGMA)).exp()
}

/// Least-squares RBF weights so the curve passes as close as it can to `gains` (EV)
/// at the nodes −8…0 EV (normal equations, Gaussian elimination in f64).
fn solve(gains: [f64; 9]) -> [f32; 8] {
    let a = |i: usize, j: usize| rbf(i as f64 - 8.0, j);
    let mut m = [[0.0f64; 9]; 8];
    for (r, row) in m.iter_mut().enumerate() {
        *row = std::array::from_fn(|c| (0..9).map(|i| a(i, r) * if c < 8 { a(i, c) } else { gains[i] }).sum());
    }
    for col in 0..8 {
        let pivot = (col..8).max_by(|&x, &y| m[x][col].abs().total_cmp(&m[y][col].abs())).expect("rows");
        m.swap(col, pivot);
        for r in 0..8 {
            if r != col {
                let k = m[r][col] / m[col][col];
                for c in col..9 {
                    m[r][c] -= k * m[col][c];
                }
            }
        }
    }
    std::array::from_fn(|j| (m[j][8] / m[j][j]) as f32)
}

/// The gain at mask exposure `ev`, as `tones.wgsl` computes it.
#[cfg(test)]
pub(crate) fn gain(ev: f32, f: &[f32; 8]) -> f32 {
    let ev = f64::from(ev.clamp(-8.0, 0.0));
    let g: f64 = (0..8).map(|j| rbf(ev, j) * f64::from(f[j])).sum();
    (g as f32).clamp(-2.0, 2.0).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_unity_everywhere() {
        let f = factors(0.0, 0.0);
        assert_eq!(f, [0.0; 8]);
        // One step off neutral moves no tone by more than that step's share.
        let f = factors(1.0, 0.0);
        for k in 0..=80 {
            let g = gain(-8.0 + k as f32 / 10.0, &f).log2();
            assert!(g.abs() < 0.02, "{g} EV at {k}");
        }
    }

    #[test]
    fn sliders_reach_their_nodes_and_leave_the_others() {
        // The least-squares fit lands within 0.25 EV of its nodes (numpy: worst 0.18 EV, at −2 EV).
        let near = |ev: f32, f: &[f32; 8], want: f32| {
            let got = gain(ev, f).log2();
            assert!((got - want).abs() < 0.25, "{got} at {ev} EV, {want} expected");
        };
        let f = factors(100.0, 0.0);
        near(-7.0, &f, 5.0 / 3.0);
        near(-6.0, &f, 5.0 / 3.0);
        near(-1.0, &f, 0.0);
        let f = factors(0.0, -100.0);
        near(-2.0, &f, -5.0 / 3.0);
        near(-1.0, &f, -5.0 / 3.0);
        near(-7.0, &f, 0.0);
    }

    #[test]
    fn gain_is_monotone_in_the_slider() {
        let at = |v: f32| gain(-6.0, &factors(v, 0.0));
        let g: Vec<f32> = (-10..=10).map(|k| at(k as f32 * 10.0)).collect();
        assert!(g.windows(2).all(|w| w[1] > w[0]), "{g:?}");
    }
}
