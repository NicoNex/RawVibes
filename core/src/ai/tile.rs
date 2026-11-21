//! A whole RAW through UtNet2, tile by tile (passes `ai_*` of `raw.wgsl`):
//! overlapping tiles, each core written back into the camera-RGB level 0
//! that `load_raw` would otherwise fill. Bayer sensors go through the Bayer
//! net (its own demosaic, replacing RCD); others through the linear net, on
//! their demosaiced pixels in linear Rec.2020 without white balance.

use rayon::prelude::*;

use std::sync::atomic::{AtomicBool, Ordering};

use super::{UNet, Variant};
use crate::gpu::Gpu;
use crate::raw::{Pixels, RawData, grid, rows};
use rawler::imgop::matrix::{multiply_row1, pseudo_inverse};

/// Tile pixels (packed for Bayer) each tile reads beyond its core on every side.
const OVERLAP: u32 = 32;

/// `AiParams` of `raw.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AiParams {
    ox: u32,
    oy: u32,
    pw: u32,
    ph: u32,
    tx: i32,
    ty: i32,
    tw: u32,
    th: u32,
    s: u32,
    _pad: [u32; 3],
    core: [u32; 4],
    rect: [u32; 4],
    to_cam: [[f32; 4]; 3],
}

/// The largest tile edge (packed pixels) whose biggest UtNet2 buffer, the
/// first decoder concat (64 channels × T² f32), the GPU can bind. At most
/// 512: a tile's activations take ~1132·T² bytes (300 MB), which a 2 GB GPU
/// holds next to the photo, and a tile keeps the worker busy for ~60 ms.
pub(crate) fn tile_edge(max_binding: u64) -> Result<u32, String> {
    [512, 256]
        .into_iter()
        .find(|&t| 64 * 4 * (t as u64).pow(2) <= max_binding)
        .ok_or_else(|| "GPU too small for AI denoise".to_string())
}

/// The net for `raw`'s sensor.
pub(crate) fn variant(raw: &RawData) -> Variant {
    if matches!(raw.pixels, Pixels::Bayer(_)) { Variant::Bayer } else { Variant::Linear }
}

/// First (x, y) of the RGGB-aligned area: R there, B one step down-right.
fn rggb_origin(raw: &RawData) -> Option<(u32, u32)> {
    let colour = |x: u32, y: u32| raw.pattern[(y % 2 * raw.pattern_size[0] + x % 2) as usize];
    (raw.pattern_size == [2, 2])
        .then(|| {
            [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().find(|&(x, y)| colour(x, y) == 0 && colour(x + 1, y + 1) == 2)
        })
        .flatten()
}

/// A tiled denoise under way, one tile per [`Gpu::ai_step`]: the worker
/// handles other jobs between tiles.
pub(crate) struct AiRun {
    pub(crate) variant: Variant,
    /// The result, complete once [`AiRun::done`].
    pub(crate) level0: wgpu::Buffer,
    /// What the net reads (the reconstructed mosaic, or un-balanced
    /// Rec.2020 pixels), the `RawParams`, one tile's input and its gain.
    source: wgpu::Buffer,
    params: wgpu::Buffer,
    tensor: wgpu::Buffer,
    gain: wgpu::Buffer,
    /// Alignment and tile size; the position is set per tile.
    tile: AiParams,
    /// The visible area of the sensor.
    crop: [u32; 4],
    /// Tiles across and down, their cores' size, and the next tile.
    tiles: (u32, u32),
    core: (u32, u32),
    next: u32,
}

impl AiRun {
    /// Fraction of the tiles done.
    pub(crate) fn progress(&self) -> f32 {
        self.next as f32 / (self.tiles.0 * self.tiles.1) as f32
    }

    pub(crate) fn done(&self) -> bool {
        self.next == self.tiles.0 * self.tiles.1
    }
}

impl Gpu {
    /// `raw` denoised by `net` (the [`variant`] for it) into a level 0 laid out
    /// as `load_raw`'s, or `None` when `cancel` was set. `progress` gets
    /// the fraction done after each tile.
    pub fn ai_denoise(
        &self,
        raw: &RawData,
        net: &UNet,
        cancel: &AtomicBool,
        progress: &dyn Fn(f32),
    ) -> Result<Option<wgpu::Buffer>, String> {
        self.ai_denoise_tiled(raw, net, self.ai_tile_edge()?, cancel, progress)
    }

    pub(crate) fn ai_tile_edge(&self) -> Result<u32, String> {
        let l = self.device.limits();
        tile_edge(l.max_storage_buffer_binding_size.min(l.max_buffer_size))
    }

    pub(crate) fn ai_denoise_tiled(
        &self,
        raw: &RawData,
        net: &UNet,
        t: u32,
        cancel: &AtomicBool,
        progress: &dyn Fn(f32),
    ) -> Result<Option<wgpu::Buffer>, String> {
        let mut run = self.ai_start(raw, t)?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.ai_step(&mut run, net)?;
            progress(run.progress());
            if run.done() {
                return Ok(Some(run.level0));
            }
        }
    }

    /// Uploads what the net reads of `raw` for a run with tiles of at most `t` pixels.
    pub(crate) fn ai_start(&self, raw: &RawData, t: u32) -> Result<AiRun, String> {
        let [_, _, cw, ch] = raw.crop;
        self.fits(cw as u64 * ch as u64 * 8)?;
        let g = raw.camera.as_shot;
        let diag = |m: [[f32; 3]; 3]| rows(std::array::from_fn(|r| m[r].map(|v| v * g[r])));
        let (source, ox, oy, s, to_cam) = match &raw.pixels {
            Pixels::Bayer(mosaic) => {
                let (ox, oy) = rggb_origin(raw).ok_or("AI denoise needs an RGGB-type Bayer sensor")?;
                let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
                (self.reconstruct_keep(raw, mosaic)?.0, ox, oy, 2, diag(id))
            }
            Pixels::Rgb(px) => {
                self.fits(px.len() as u64 * 12)?;
                // The linear net was trained on Rec.2020 without white balance.
                let m = raw.camera.cam2rec2020;
                let lin: Vec<[f32; 3]> = px
                    .par_iter()
                    .map(|p| {
                        let c = [p[0] / g[0], p[1] / g[1], p[2] / g[2]];
                        multiply_row1(&m, &c)
                    })
                    .collect();
                let back = pseudo_inverse(m);
                (self.storage(bytemuck::cast_slice(&lin)), 0, 0, 1, diag(back))
            }
        };
        let (pw, ph) = ((raw.width - ox) / s, (raw.height - oy) / s);
        // As few tiles as edge `t` allows, then as small as they can be: less overlap to compute.
        let fit = |n: u32| {
            let tiles = n.div_ceil(t - 2 * OVERLAP);
            let core = n.div_ceil(tiles);
            (tiles, core, (core + 2 * OVERLAP).next_multiple_of(16))
        };
        let ((nx, cx, tw), (ny, cy, th)) = (fit(pw), fit(ph));
        let channels = if s == 2 { 4 } else { 3 };
        Ok(AiRun {
            variant: variant(raw),
            level0: self.buffer(cw as u64 * ch as u64 * 8, wgpu::BufferUsages::COPY_SRC),
            source,
            params: self.raw_params(raw, [0; 4]),
            tensor: self.buffer(channels * tw as u64 * th as u64 * 4, wgpu::BufferUsages::empty()),
            gain: self.buffer(4, wgpu::BufferUsages::empty()),
            tile: AiParams {
                ox,
                oy,
                pw,
                ph,
                tx: 0,
                ty: 0,
                tw,
                th,
                s,
                _pad: [0; 3],
                core: [0; 4],
                rect: [0; 4],
                to_cam,
            },
            crop: raw.crop,
            tiles: (nx, ny),
            core: (cx, cy),
            next: 0,
        })
    }

    /// Runs the next tile of `run` and waits for it.
    pub(crate) fn ai_step(&self, run: &mut AiRun, net: &UNet) -> Result<(), String> {
        debug_assert!(self.raw.batch.borrow().is_none(), "AI denoise waits per tile: no batch may be open");
        let AiParams { ox, oy, pw, ph, tw, th, s, .. } = run.tile;
        let (sx, sy) = run.core;
        let (cx, cy) = (run.next % run.tiles.0 * sx, run.next / run.tiles.0 * sy);
        let core = [cx, cy, (cx + sx).min(pw), (cy + sy).min(ph)];
        // The core in visible-area pixels; edge tiles also take what lies outside the aligned area.
        let [x, y, w, h] = run.crop;
        let at = |c: u32, o: u32, crop: u32, end: u32, n: u32| match c {
            0 => 0,
            c if c == n => end,
            c => (o + s * c).saturating_sub(crop).min(end),
        };
        let rect = [
            at(core[0], ox, x, w, pw),
            at(core[1], oy, y, h, ph),
            at(core[2], ox, x, w, pw),
            at(core[3], oy, y, h, ph),
        ];
        let tile = AiParams { tx: cx as i32 - OVERLAP as i32, ty: cy as i32 - OVERLAP as i32, core, rect, ..run.tile };
        let ai = self.storage(bytemuck::bytes_of(&tile));
        let (p, r) = (&self.raw, &*run);
        let n = (r.tensor.size() / 4) as u32;
        self.passes(&[(&p.ai_pack, &[(0, &r.params), (2, &r.source), (12, &ai), (13, &r.tensor)], grid(n))]);
        let out = net.run(self, &r.tensor, tw, th);
        self.passes(&[
            (&p.ai_gain, &[(12, &ai), (13, &r.tensor), (14, &out), (15, &r.gain)], (1, 1)),
            (
                &p.ai_unpack,
                &[(0, &r.params), (10, &r.level0), (12, &ai), (14, &out), (15, &r.gain)],
                grid((rect[2] - rect[0]) * (rect[3] - rect[1])),
            ),
        ]);
        // Also frees this tile's activations before the next allocates its own.
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| format!("GPU lost: {e}"))?;
        run.next += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{Weights, models_dir};
    use crate::develop::Camera;
    use crate::raw::unpack;

    fn bayer(w: u32, h: u32, as_shot: [f32; 3], f: impl Fn(u32, u32) -> f32) -> RawData {
        let mut pattern = [0; 36];
        pattern[..4].copy_from_slice(&[0, 1, 1, 2]);
        let mut camera = Camera::neutral();
        camera.as_shot = as_shot;
        RawData {
            pixels: Pixels::Bayer((0..w * h).map(|i| f(i % w, i / w)).collect()),
            width: w,
            height: h,
            pattern,
            pattern_size: [2, 2],
            crop: [0, 0, w, h],
            orientation: 1,
            camera,
        }
    }

    /// GPU and Bayer net, or why the test is skipped.
    fn setup() -> Option<(Gpu, UNet)> {
        setup_for(Variant::Bayer)
    }

    fn setup_for(variant: Variant) -> Option<(Gpu, UNet)> {
        let Some(dir) = models_dir() else {
            eprintln!("no models, skipped");
            return None;
        };
        let Ok(gpu) = Gpu::new() else {
            eprintln!("no GPU, skipped");
            return None;
        };
        let net = UNet::new(&gpu, &Weights::load(&dir, variant).unwrap());
        Some((gpu, net))
    }

    /// Level 0 as f32 RGB.
    fn run(gpu: &Gpu, net: &UNet, raw: &RawData, t: u32) -> Vec<[f32; 3]> {
        let buf = gpu.ai_denoise_tiled(raw, net, t, &AtomicBool::new(false), &|_| {}).unwrap().unwrap();
        let words: Vec<u32> = bytemuck::cast_slice(&gpu.read_f32(&buf)).to_vec();
        words.as_chunks::<2>().0.iter().map(unpack).collect()
    }

    /// Deterministic noise in −1..1 (a hash).
    fn noise(x: u32, y: u32) -> f32 {
        let h = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
        (h >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }

    fn mean_std(v: &[f32]) -> (f32, f32) {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (m, (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt())
    }

    fn green(px: &[[f32; 3]]) -> Vec<f32> {
        px.iter().map(|p| p[1]).collect()
    }

    #[test]
    fn small_gpu_limit_uses_small_tiles() {
        assert_eq!(tile_edge(u64::MAX), Ok(512));
        assert_eq!(tile_edge(64 << 20), Ok(512));
        assert_eq!(tile_edge(63 << 20), Ok(256));
        assert!(tile_edge(1 << 20).is_err());
    }

    #[test]
    fn rggb_origin_finds_every_phase() {
        let mut raw = bayer(4, 4, [1.0; 3], |_, _| 0.0);
        for (cells, origin) in
            [([0, 1, 1, 2], (0, 0)), ([1, 0, 2, 1], (1, 0)), ([1, 2, 0, 1], (0, 1)), ([2, 1, 1, 0], (1, 1))]
        {
            raw.pattern[..4].copy_from_slice(&cells);
            assert_eq!(rggb_origin(&raw), Some(origin));
        }
    }

    /// A smooth ramp over 3×3 tiles of 256: no step at tile borders.
    #[test]
    fn tiles_have_no_seams() {
        let Some((gpu, net)) = setup() else { return };
        let (w, h) = (1080, 1080);
        let raw = bayer(w, h, [1.0; 3], |x, y| 0.1 + 0.3 * (x + y) as f32 / (w + h) as f32);
        let g = green(&run(&gpu, &net, &raw, 256));
        let at = |x: u32, y: u32| g[(y * w + x) as usize];
        // Borders between cores, at full resolution: three tiles each way, cores fitted to the photo.
        let core = (w / 2).div_ceil(3);
        let mut worst = 0f32;
        for k in 1..3 {
            let b = 2 * core * k;
            for i in (8..h.min(w) - 8).step_by(7) {
                // A jump beyond the ramp's own slope, both directions.
                let (jx, sx) = ((at(b, i) - at(b - 1, i)).abs(), (at(b - 1, i) - at(b - 2, i)).abs());
                let (jy, sy) = ((at(i, b) - at(i, b - 1)).abs(), (at(i, b - 1) - at(i, b - 2)).abs());
                worst = worst.max((jx - sx) / at(b, i)).max((jy - sy) / at(i, b));
            }
        }
        assert!(worst <= 0.01, "seam {worst}");
    }

    #[test]
    fn ai_keeps_the_mean() {
        let Some((gpu, net)) = setup() else { return };
        // A grey card shot under a warm light: unbalanced red and blue.
        let gains = [2.0, 1.0, 1.5];
        let raw = bayer(512, 512, gains, |x, y| {
            let c = [[0, 1], [1, 2]][y as usize % 2][x as usize % 2];
            (0.2 + 0.02 * noise(x, y)) / gains[c]
        });
        let px = run(&gpu, &net, &raw, 256);
        for c in 0..3 {
            let (m, _) = mean_std(&px.iter().map(|p| p[c]).collect::<Vec<_>>());
            assert!((m / 0.2 - 1.0).abs() < 0.01, "channel {c} mean {m}");
        }
    }

    #[test]
    fn ai_reduces_noise() {
        let Some((gpu, net)) = setup() else { return };
        // Poisson–Gaussian: σ² = a·v + b.
        let sigma = (0.002f32 * 0.2 + 1e-5).sqrt();
        let raw = bayer(512, 512, [1.0; 3], |x, y| 0.2 + sigma * 1.7 * noise(x, y));
        let (_, before) = mean_std(&bayer_green(&raw));
        let (_, after) = mean_std(&green(&run(&gpu, &net, &raw, 256)));
        assert!(after * 3.0 <= before, "std {before} → {after}");
    }

    fn bayer_green(raw: &RawData) -> Vec<f32> {
        let Pixels::Bayer(m) = &raw.pixels else { unreachable!() };
        m.iter().enumerate().filter(|(i, _)| (i % 512 + i / 512) % 2 == 1).map(|(_, v)| *v).collect()
    }

    /// X-Trans and other demosaiced RAWs: the linear net, colour and mean kept.
    #[test]
    fn linear_variant_reduces_noise_on_rgb_raw() {
        let Some((gpu, net)) = setup_for(Variant::Linear) else { return };
        let (w, h) = (512u32, 384u32);
        let grey = [0.25, 0.2, 0.15];
        let px: Vec<[f32; 3]> =
            (0..w * h).map(|i| grey.map(|v| v + 0.03 * noise(i % w, i / w + 7 * (v * 100.0) as u32))).collect();
        let mut camera = Camera::neutral();
        camera.as_shot = [2.0, 1.0, 1.5];
        let raw = RawData {
            pixels: Pixels::Rgb(px.clone()),
            width: w,
            height: h,
            pattern: [0; 36],
            pattern_size: [1, 1],
            crop: [0, 0, w, h],
            orientation: 1,
            camera,
        };
        assert_eq!(variant(&raw), Variant::Linear);
        let out = run(&gpu, &net, &raw, 256);
        for c in 0..3 {
            let (m0, s0) = mean_std(&px.iter().map(|p| p[c]).collect::<Vec<_>>());
            let (m1, s1) = mean_std(&out.iter().map(|p| p[c]).collect::<Vec<_>>());
            assert!((m1 / m0 - 1.0).abs() < 0.01, "channel {c}: mean {m0} → {m1}");
            assert!(s1 * 3.0 <= s0, "channel {c}: std {s0} → {s1}");
        }
    }

    #[test]
    fn dark_input_stays_finite() {
        let Some((gpu, net)) = setup() else { return };
        for v in [0.0, 1e-4] {
            let px = run(&gpu, &net, &bayer(256, 256, [2.0, 1.0, 1.5], |_, _| v), 256);
            assert!(px.iter().flatten().all(|c| c.is_finite() && *c >= 0.0), "input {v}");
        }
    }

    #[test]
    fn cancel_returns_none() {
        let Some((gpu, net)) = setup() else { return };
        let raw = bayer(256, 256, [1.0; 3], |_, _| 0.2);
        assert!(gpu.ai_denoise(&raw, &net, &AtomicBool::new(true), &|_| {}).unwrap().is_none());
    }

    /// `RAWVIBES_RAW=<file.rw2>`: time of a whole sample photo.
    #[test]
    fn ai_on_sample() {
        let Ok(path) = std::env::var("RAWVIBES_RAW") else { return };
        let Some((gpu, net)) = setup() else { return };
        let raw = crate::raw::read_raw(std::path::Path::new(&path)).unwrap();
        let start = std::time::Instant::now();
        gpu.ai_denoise(&raw, &net, &AtomicBool::new(false), &|_| {}).unwrap().unwrap();
        eprintln!("AI denoise {}×{}: {:.2?}", raw.width, raw.height, start.elapsed());
    }
}

/// Measurements, not checks: `cargo test --release ai_vs_classic -- --ignored --nocapture`.
#[cfg(test)]
mod quality {
    use super::*;
    use crate::ai::{Weights, models_dir};
    use crate::develop::{Camera, Detail, Develop};
    use crate::profiles::Profile;
    use crate::view::Region;

    const W: u32 = 768;
    const H: u32 = 512;

    /// Ground truth, balanced camera RGB: flat patches (top), a chirp
    /// texture (middle), colour edges (bottom).
    fn scene(x: u32, y: u32) -> [f32; 3] {
        let (xf, yf) = (x as f32, y as f32);
        match y * 3 / H {
            0 => [[0.03f32; 3], [0.12; 3], [0.45; 3]][(x * 3 / W) as usize],
            1 => {
                let t = 0.5 + 0.5 * (xf * xf / W as f32 * 0.25).sin() * (yf * 0.15).cos();
                [0.1 + 0.25 * t; 3]
            }
            _ => {
                let i = (x / 48 + y / 48) % 4;
                [[0.3, 0.1, 0.05], [0.05, 0.2, 0.1], [0.08, 0.1, 0.35], [0.25, 0.25, 0.25]][i as usize]
            }
        }
    }

    /// Linear Rec.2020 of `raw` developed 1:1 with `dev`.
    fn develop(gpu: &Gpu, raw: &crate::raw::GpuRaw, dev: &Develop) -> Vec<[f32; 4]> {
        let r = Region { center: [W as f32 / 2.0, H as f32 / 2.0], size: [W as f32, H as f32], angle: 0.0 };
        let p = gpu.develop(raw, &r, W, H, dev, Profile::Standard).unwrap();
        gpu.read_lin(&p)
    }

    /// PSNR (peak 1 after exposure) of `a` against `b` over rows `ys`, 16 px in from the sides.
    fn psnr(a: &[[f32; 4]], b: &[[f32; 4]], ys: std::ops::Range<u32>) -> f32 {
        let (mut se, mut n, mut peak) = (0f64, 0f64, 0f32);
        for y in ys.start + 16..ys.end - 16 {
            for x in 16..W - 16 {
                let i = (y * W + x) as usize;
                for c in 0..3 {
                    se += ((a[i][c] - b[i][c]) as f64).powi(2);
                    peak = peak.max(b[i][c]);
                }
                n += 3.0;
            }
        }
        10.0 * ((peak as f64).powi(2) / (se / n)).log10() as f32
    }

    #[test]
    #[ignore]
    fn ai_vs_classic() {
        let (Some(dir), Ok(gpu)) = (models_dir(), Gpu::new()) else { return };
        let net = UNet::new(&gpu, &Weights::load(&dir, Variant::Bayer).unwrap());
        let gains = [2.0f32, 1.0, 1.5];
        let colour = |x: u32, y: u32| [[0, 1], [1, 2]][y as usize % 2][x as usize % 2];
        let mosaic = |noise: Option<(f32, f32)>| {
            let mut n = crate::noise::tests::normals(7);
            let mut camera = Camera::neutral();
            camera.as_shot = gains;
            let px = (0..W * H)
                .map(|i| {
                    let (x, y) = (i % W, i / W);
                    let v = scene(x, y)[colour(x, y)] / gains[colour(x, y)];
                    noise.map_or(v, |(a, b)| v + (a * v + b).sqrt() * n())
                })
                .collect();
            let mut pattern = [0; 36];
            pattern[..4].copy_from_slice(&[0, 1, 1, 2]);
            RawData {
                pixels: Pixels::Bayer(px),
                width: W,
                height: H,
                pattern,
                pattern_size: [2, 2],
                crop: [0, 0, W, H],
                orientation: 1,
                camera,
            }
        };
        let truth = develop(&gpu, &gpu.load_raw(&mosaic(None)).unwrap(), &Develop::default());
        let bands = [("flat", 0..H / 3), ("texture", H / 3..2 * H / 3), ("colour", 2 * H / 3..H)];
        for (name, noise) in [("low", (2e-4, 2e-6)), ("mid", (1e-3, 1e-5)), ("high", (4e-3, 5e-5))] {
            let data = mosaic(Some(noise));
            let mut raw = gpu.load_raw(&data).unwrap();
            let level0 = gpu.ai_denoise(&data, &net, &AtomicBool::new(false), &|_| {}).unwrap().unwrap();
            // H1: darktable's way, the net's value at each site's own colour, then RCD.
            let words: Vec<u32> = bytemuck::cast_slice(&gpu.read_f32(&level0)).to_vec();
            let ai_rgb: Vec<[f32; 3]> = words.as_chunks::<2>().0.iter().map(crate::raw::unpack).collect();
            let remosaic = RawData {
                pixels: Pixels::Bayer(
                    (0..W * H)
                        .map(|i| ai_rgb[i as usize][colour(i % W, i / W)] / gains[colour(i % W, i / W)])
                        .collect(),
                ),
                camera: data.camera.clone(),
                ..data
            };
            let mut via_rcd = gpu.load_raw(&remosaic).unwrap();
            raw.ai = Some(gpu.mips(&data, level0));
            via_rcd.ai = None;
            let mut rows = vec![];
            let detail = |luma: f32, chroma: f32, ai: Option<f32>| Develop {
                detail: Detail {
                    nr_luma: luma,
                    nr_chroma: chroma,
                    nr_ai: ai.is_some(),
                    nr_ai_strength: ai.unwrap_or(100.0),
                    ..Detail::default()
                },
                ..Develop::default()
            };
            let mut img = image::RgbImage::new(5 * W / 2, H / 3);
            let mut save = |j: u32, v: &[[f32; 4]]| {
                // The colour band's left half, linear → sRGB-ish gamma.
                for y in 0..H / 3 {
                    for x in 0..W / 2 {
                        let px = v[((y + 2 * H / 3) * W + x) as usize];
                        let rgb = [0, 1, 2].map(|c| (px[c].clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8);
                        img.put_pixel(j * W / 2 + x, y, image::Rgb(rgb));
                    }
                }
            };
            save(0, &truth);
            save(1, &develop(&gpu, &raw, &detail(0.0, 0.0, None)));
            save(2, &develop(&gpu, &raw, &detail(50.0, 50.0, None)));
            save(3, &develop(&gpu, &raw, &detail(0.0, 0.0, Some(100.0))));
            let remosaiced = develop(&gpu, &via_rcd, &detail(0.0, 0.0, None));
            save(4, &remosaiced);
            // Colour band: mean of each colour's square centres (bias across the whole patch).
            let ai_out = develop(&gpu, &raw, &detail(0.0, 0.0, Some(100.0)));
            for (k, name) in ["orange", "green", "blue", "grey"].iter().enumerate() {
                let mean = |v: &[[f32; 4]]| {
                    let (mut m, mut n) = ([0f32; 3], 0f32);
                    for y in 2 * H / 3 + 8..H - 8 {
                        for x in 8..W - 8 {
                            let (cx, cy) = (x % 48, y % 48);
                            if (x / 48 + y / 48) % 4 == k as u32 && (12..36).contains(&cx) && (12..36).contains(&cy) {
                                (0..3).for_each(|c| m[c] += v[(y * W + x) as usize][c]);
                                n += 1.0;
                            }
                        }
                    }
                    m.map(|v| v / n)
                };
                let (t, a) = (mean(&truth), mean(&ai_out));
                eprintln!("    {name:<6} truth {t:.4?}  AI/truth {:.3?}", [0, 1, 2].map(|c| a[c] / t[c]));
            }
            let p: Vec<String> =
                bands.iter().map(|(_, ys)| format!("{:5.1}", psnr(&remosaiced, &truth, ys.clone()))).collect();
            eprintln!("  {:<20} {}", "AI remosaic + RCD", p.join("  "));
            if let Ok(out) = std::env::var("RAWVIBES_OUT") {
                img.save(format!("{out}/synthetic_{name}.png")).unwrap();
            }
            for (label, dev) in [
                ("none", detail(0.0, 0.0, None)),
                ("classic 25/25", detail(25.0, 25.0, None)),
                ("classic 50/50", detail(50.0, 50.0, None)),
                ("classic 80/50", detail(80.0, 50.0, None)),
                ("AI 100", detail(0.0, 0.0, Some(100.0))),
                ("AI 100 +chroma 25", detail(0.0, 25.0, Some(100.0))),
                ("AI 60", detail(0.0, 0.0, Some(60.0))),
            ] {
                let out = develop(&gpu, &raw, &dev);
                let p: Vec<String> =
                    bands.iter().map(|(_, ys)| format!("{:5.1}", psnr(&out, &truth, ys.clone()))).collect();
                rows.push(format!("  {label:<20} {}", p.join("  ")));
            }
            eprintln!("noise {name}: PSNR dB  flat  texture  colour\n{}", rows.join("\n"));
            // Mean per channel over each flat patch's centre: bias, not noise.
            let ai = develop(&gpu, &raw, &detail(0.0, 0.0, Some(100.0)));
            let none = develop(&gpu, &raw, &detail(0.0, 0.0, None));
            for k in 0..3u32 {
                let mean = |v: &[[f32; 4]]| {
                    let mut m = [0f32; 3];
                    let (x0, n) = (k * W / 3 + 40, ((W / 3 - 80) * (H / 3 - 80)) as f32);
                    for y in 40..H / 3 - 40 {
                        for x in x0..x0 + W / 3 - 80 {
                            (0..3).for_each(|c| m[c] += v[(y * W + x) as usize][c] / n);
                        }
                    }
                    m
                };
                let std = |v: &[[f32; 4]]| {
                    let m = mean(v);
                    let (x0, mut sum, mut n) = (k * W / 3 + 40, 0f32, 0f32);
                    for y in 40..H / 3 - 40 {
                        for x in x0..x0 + W / 3 - 80 {
                            sum += (v[(y * W + x) as usize][1] - m[1]).powi(2);
                            n += 1.0;
                        }
                    }
                    (sum / n).sqrt() / m[1]
                };
                let classic = develop(&gpu, &raw, &detail(50.0, 50.0, None));
                eprintln!(
                    "  patch {k}: G std/mean  none {:.4}  classic50 {:.4}  AI {:.4}",
                    std(&none),
                    std(&classic),
                    std(&ai)
                );
            }
        }
    }

    /// `RAWVIBES_RAW=<file> RAWVIBES_OUT=<dir>`: 1:1 crops none | classic 50/50 | AI, side by side.
    #[test]
    #[ignore]
    fn ai_crops() {
        let (Ok(path), Ok(out), Some(dir), Ok(gpu)) =
            (std::env::var("RAWVIBES_RAW"), std::env::var("RAWVIBES_OUT"), models_dir(), Gpu::new())
        else {
            return;
        };
        let data = crate::raw::read_raw(std::path::Path::new(&path)).unwrap();
        let net = UNet::new(&gpu, &Weights::load(&dir, Variant::Bayer).unwrap());
        let mut raw = gpu.load_raw(&data).unwrap();
        let level0 = gpu.ai_denoise(&data, &net, &AtomicBool::new(false), &|_| {}).unwrap().unwrap();
        raw.ai = Some(gpu.mips(&data, level0));
        let (fw, fh) = raw.oriented_size();
        let c = 384u32;
        let dev = |luma: f32, chroma: f32, ai: bool| Develop {
            exposure: 0.5,
            detail: Detail { nr_luma: luma, nr_chroma: chroma, nr_ai: ai, ..Detail::default() },
            ..Develop::default()
        };
        let devs = [dev(0.0, 0.0, false), dev(50.0, 50.0, false), dev(0.0, 0.0, true)];
        for (k, centre) in [(0, [fw / 2, fh / 2]), (1, [fw / 4, 3 * fh / 4]), (2, [3 * fw / 4, fh / 4])].iter() {
            let r = Region { center: centre.map(|v| v as f32), size: [c as f32; 2], angle: 0.0 };
            let mut img = image::RgbImage::new(3 * c, c);
            for (j, d) in devs.iter().enumerate() {
                let p = gpu.read_photo(&gpu.develop(&raw, &r, c, c, d, Profile::Standard).unwrap());
                for (i, px) in p.pixels.iter().enumerate() {
                    let rgb = image::Rgb([0, 1, 2].map(|ch| (px[ch].clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
                    img.put_pixel(j as u32 * c + i as u32 % c, i as u32 / c, rgb);
                }
            }
            img.save(format!("{out}/crop{k}.png")).unwrap();
        }
    }
}
