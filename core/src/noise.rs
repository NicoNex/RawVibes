//! Sensor noise model: the Poisson–Gaussian variance `σ² = a·x + b` of the
//! as-shot camera RGB, estimated per photo from its flattest areas (after
//! Foi et al., "Practical Poissonian-Gaussian noise modeling", 2008).

/// `σ² = a·x + b` for a camera RGB value `x` (green channel).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NoiseModel {
    pub a: f32,
    pub b: f32,
}

const BLOCK: usize = 16;
const BINS: usize = 16;

/// Statistics of one 16×16 block: mean, noise variance (from the median
/// absolute deviation of a high-pass) and texture (variance of its 4×4
/// sub-block means, which noise barely reaches).
struct Block {
    mean: f32,
    noise: f32,
    texture: f32,
}

fn block(g: impl Fn(usize, usize) -> f32, x0: usize, y0: usize) -> Block {
    let mut sum = 0.0;
    let mut d = Vec::with_capacity((BLOCK - 1) * (BLOCK - 1));
    let mut sub = [0.0f32; 16];
    for y in 0..BLOCK {
        for x in 0..BLOCK {
            let v = g(x0 + x, y0 + y);
            sum += v;
            sub[(y / 4) * 4 + x / 4] += v / 16.0;
            if x + 1 < BLOCK && y + 1 < BLOCK {
                // Zero for any plane: only noise is left, with variance 6σ².
                d.push(((2.0 * v - g(x0 + x + 1, y0 + y) - g(x0 + x, y0 + y + 1)) / 6f32.sqrt()).abs());
            }
        }
    }
    let sigma = 1.4826 * median(&mut d);
    let sm = sub.iter().sum::<f32>() / 16.0;
    let texture = sub.iter().map(|s| (s - sm).powi(2)).sum::<f32>() / 16.0;
    Block { mean: sum / (BLOCK * BLOCK) as f32, noise: sigma * sigma, texture }
}

fn median(v: &mut [f32]) -> f32 {
    let mid = v.len() / 2;
    *v.select_nth_unstable_by(mid, f32::total_cmp).1
}

/// The noise model of `px` (camera RGB, `w` × `h`), from the green channel:
/// blocks binned by brightness, the 5% least textured of each bin (at least
/// 16) give that bin's variance, and a weighted line fits `σ² = a·x + b`.
pub fn estimate(px: &[[f32; 3]], w: usize, h: usize) -> NoiseModel {
    let g = |x: usize, y: usize| px[y * w + x][1];
    let clipped = |x0: usize, y0: usize| (0..BLOCK).any(|y| (0..BLOCK).any(|x| px[(y0 + y) * w + x0 + x][1] >= 0.95));
    let blocks: Vec<Block> = (0..h / BLOCK)
        .flat_map(|by| (0..w / BLOCK).map(move |bx| (bx * BLOCK, by * BLOCK)))
        .filter(|&(x, y)| !clipped(x, y))
        .map(|(x, y)| block(g, x, y))
        .collect();
    let top = blocks.iter().map(|b| b.mean).fold(0.0, f32::max).max(1e-6);
    let mut bins: Vec<Vec<&Block>> = (0..BINS).map(|_| Vec::new()).collect();
    for b in &blocks {
        bins[((b.mean / top * BINS as f32) as usize).min(BINS - 1)].push(b);
    }
    // (mean, variance, weight) per bin.
    let points: Vec<(f32, f32, f32)> = bins
        .iter_mut()
        .filter(|bin| bin.len() >= 16)
        .map(|bin| {
            bin.sort_by(|p, q| p.texture.total_cmp(&q.texture));
            let flat = &bin[..(bin.len() / 20).max(16).min(bin.len())];
            let mut means: Vec<f32> = flat.iter().map(|b| b.mean).collect();
            let mut noise: Vec<f32> = flat.iter().map(|b| b.noise).collect();
            (median(&mut means), median(&mut noise), flat.len() as f32)
        })
        .collect();
    let sw: f32 = points.iter().map(|p| p.2).sum();
    if points.len() < 2 || sw <= 0.0 {
        let b = points.first().map_or(0.0, |p| p.1);
        return NoiseModel { a: 0.0, b };
    }
    let mx = points.iter().map(|p| p.0 * p.2).sum::<f32>() / sw;
    let my = points.iter().map(|p| p.1 * p.2).sum::<f32>() / sw;
    let sxx: f32 = points.iter().map(|p| p.2 * (p.0 - mx).powi(2)).sum();
    let sxy: f32 = points.iter().map(|p| p.2 * (p.0 - mx) * (p.1 - my)).sum();
    let a = if sxx > 0.0 { (sxy / sxx).max(0.0) } else { 0.0 };
    NoiseModel { a, b: (my - a * mx).max(0.0) }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deterministic standard normal samples (xorshift + Box–Muller).
    pub(crate) fn normals(seed: u32) -> impl FnMut() -> f32 {
        let mut x = seed.max(1);
        let mut uniform = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 + 1.0) / (u32::MAX as f32 + 2.0)
        };
        move || (-2.0 * uniform().ln()).sqrt() * (std::f32::consts::TAU * uniform()).cos()
    }

    #[test]
    fn noise_model_recovers_synthetic_parameters() {
        let (w, h) = (512, 512);
        let (a, b) = (0.002, 0.0002);
        let mut n = normals(7);
        let px: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let x = 0.05 + 0.1 * ((i % w) / 64) as f32;
                std::array::from_fn(|_| x + (a * x + b).sqrt() * n())
            })
            .collect();
        let m = estimate(&px, w, h);
        assert!((m.a - a).abs() < 0.15 * a, "a {} vs {a}", m.a);
        assert!((m.b - b).abs() < 0.15 * b, "b {} vs {b}", m.b);
    }
}
