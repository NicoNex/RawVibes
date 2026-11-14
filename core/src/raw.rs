//! RAW files on the GPU: the decoded mosaic is uploaded once, white-balanced
//! as shot and its clipped highlights rebuilt (`raw.wgsl`), then demosaiced.

use std::path::Path;

use rawler::imgop::chromatic_adaption::bradford_adaption_matrix;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::matrix::{multiply, transform_1d};
use rawler::imgop::sensor::SensorType;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawPhotometricInterpretation;
use wgpu::util::DeviceExt;

use crate::develop::Camera;
use crate::gpu::{Gpu, WORKGROUP};

/// What the GPU needs from a RAW file, read on the CPU.
pub struct RawData {
    pub pixels: Pixels,
    pub width: u32,
    pub height: u32,
    /// CFA colour (0 R, 1 G, 2 B) per pattern cell, row-major, and the pattern size.
    pub pattern: [u32; 36],
    pub pattern_size: [u32; 2],
    /// Visible area of the sensor: x, y, width, height.
    pub crop: [u32; 4],
    /// EXIF orientation (1–8).
    pub orientation: u16,
    pub camera: Camera,
}

pub enum Pixels {
    /// Rescaled (0 = black, 1 = white) Bayer mosaic, not white-balanced.
    Bayer(Vec<f32>),
    /// Already demosaiced and cropped by rawler (X-Trans, linear DNG),
    /// as-shot-balanced camera RGB.
    Rgb(Vec<[f32; 3]>),
}

impl std::fmt::Debug for RawData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RawData({}×{})", self.width, self.height)
    }
}

/// Decodes `path` up to the point where the GPU takes over.
pub fn read_raw(path: &Path) -> Result<RawData, String> {
    let src = rawler::rawsource::RawSource::new(path).map_err(|e| e.to_string())?;
    let params = rawler::decoders::RawDecodeParams::default();
    let mut raw = rawler::decode(&src, &params).map_err(|e| e.to_string())?;
    // Decoders such as Panasonic's leave `raw.orientation` at Normal; the EXIF
    // Orientation tag is the reliable source (6/8 = portrait shots).
    let orientation = rawler::get_decoder(&src)
        .and_then(|d| d.raw_metadata(&src, &params))
        .ok()
        .and_then(|m| m.exif.orientation)
        .unwrap_or_else(|| raw.orientation.to_u16());
    let camera = camera(&raw);
    let (w, h) = (raw.width as u32, raw.height as u32);
    let bayer = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(c) if raw.cpp == 1 && c.cfa.is_rgb() && c.sensor == SensorType::Bayer => {
            Some(c.cfa.clone())
        }
        _ => None,
    };
    if let Some(cfa) = bayer {
        let rect = raw.crop_area.or(raw.active_area);
        raw.apply_scaling().map_err(|e| e.to_string())?;
        let mut pattern = [0; 36];
        for y in 0..cfa.height {
            for x in 0..cfa.width {
                pattern[y * cfa.width + x] = cfa.color_at(y, x).min(2) as u32;
            }
        }
        let crop = rect.map_or([0, 0, w, h], |r| [r.p.x as u32, r.p.y as u32, r.d.w as u32, r.d.h as u32]);
        return Ok(RawData {
            pixels: Pixels::Bayer(raw.data.as_f32().into_owned()),
            width: w,
            height: h,
            pattern,
            pattern_size: [cfa.width as u32, cfa.height as u32],
            crop,
            orientation,
            camera,
        });
    }
    // X-Trans, 4-colour and linear RAWs: rawler demosaics on the CPU.
    use ProcessingStep::*;
    let dev = RawDevelop::new_with(&[Rescale, Demosaic, FujiRotate, CropActiveArea, CropDefault]);
    let (rgb, w, h, camera) = match dev.develop_intermediate(&raw).map_err(|e| e.to_string())? {
        Intermediate::ThreeColor(px) => {
            let g = camera.as_shot;
            (px.data.iter().map(|p| std::array::from_fn(|c| p[c] * g[c])).collect(), px.width, px.height, camera)
        }
        // shortcut: CYGM/RGBE sensors keep rawler's 4→3 calibration, no Kelvin WB; upgrade if one is reported.
        Intermediate::FourColor(px) => {
            let cal = |p: &[f32; 4]| [p[0], 0.5 * (p[1] + p[3]), p[2]];
            (px.data.iter().map(cal).collect(), px.width, px.height, Camera::neutral())
        }
        Intermediate::Monochrome(_) => return Err("unsupported sensor layout".into()),
    };
    let (w, h) = (w as u32, h as u32);
    Ok(RawData {
        pixels: Pixels::Rgb(rgb),
        width: w,
        height: h,
        pattern: [0; 36],
        pattern_size: [1, 1],
        crop: [0, 0, w, h],
        orientation,
        camera,
    })
}

/// The camera's colour model from the file: D65-adapted colour matrix and
/// as-shot multipliers.
fn camera(raw: &rawler::RawImage) -> Camera {
    use Illuminant::*;
    let xyz2cam = raw.color_matrix_find_first([D65, A, B, C, D50, D55, D75, Daylight, Flash]).and_then(|(illu, m)| {
        let m = transform_1d::<3, 3>(m.get(..9)?)?;
        Some(if illu == D65 { m } else { multiply(&m, &bradford_adaption_matrix(&illu, &D65)) })
    });
    let as_shot = [raw.wb_coeffs[0], raw.wb_coeffs[1], raw.wb_coeffs[2]];
    match xyz2cam {
        // shortcut: DNG BaselineExposure is not exposed by rawler 0.8; 0 EV until it is.
        Some(m) => Camera::new(m, as_shot, 0.0),
        None => Camera::neutral(),
    }
}

/// A developed RAW on the GPU: as-shot-balanced camera RGB of the visible
/// area, f16, level 0 at full resolution and each next one half the size.
pub struct GpuRaw {
    pub(crate) levels: Vec<(wgpu::Buffer, u32, u32)>,
    pub camera: Camera,
    pub orientation: u16,
}

impl GpuRaw {
    /// Full-resolution size, before orientation.
    pub fn size(&self) -> (u32, u32) {
        (self.levels[0].1, self.levels[0].2)
    }
}

/// Longest side of the smallest mip level.
const MIP_MIN: u32 = 256;

/// Compute pipelines of `raw.wgsl`.
pub struct Pipelines {
    white_balance: wgpu::ComputePipeline,
    hl_partials: wgpu::ComputePipeline,
    hl_reduce: wgpu::ComputePipeline,
    hl_apply: wgpu::ComputePipeline,
    rcd_init: wgpu::ComputePipeline,
    rcd_vh: wgpu::ComputePipeline,
    rcd_green: wgpu::ComputePipeline,
    rcd_pq: wgpu::ComputePipeline,
    rcd_rb: wgpu::ComputePipeline,
    rcd_rb_green: wgpu::ComputePipeline,
    rcd_pack: wgpu::ComputePipeline,
    rgb_pack: wgpu::ComputePipeline,
    mip: wgpu::ComputePipeline,
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("raw.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("raw.wgsl").into()),
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            white_balance: pipeline("white_balance"),
            hl_partials: pipeline("hl_partials"),
            hl_reduce: pipeline("hl_reduce"),
            hl_apply: pipeline("hl_apply"),
            rcd_init: pipeline("rcd_init"),
            rcd_vh: pipeline("rcd_vh"),
            rcd_green: pipeline("rcd_green"),
            rcd_pq: pipeline("rcd_pq"),
            rcd_rb: pipeline("rcd_rb"),
            rcd_rb_green: pipeline("rcd_rb_green"),
            rcd_pack: pipeline("rcd_pack"),
            rgb_pack: pipeline("rgb_pack"),
            mip: pipeline("mip"),
        }
    }
}

/// `RawParams` of `raw.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    pw: u32,
    ph: u32,
    groups: u32,
    _pad: [u32; 3],
    clip: [f32; 4],
    crop: [u32; 4],
    size: [u32; 4],
    pattern: [u32; 36],
}

/// A compute pass: pipeline, bindings, workgroup grid.
type Pass<'a> = (&'a wgpu::ComputePipeline, &'a [(u32, &'a wgpu::Buffer)], (u32, u32));

/// Workgroups covering `n` items on a 2-D grid (≤ 65535 per dimension).
fn grid(n: u32) -> (u32, u32) {
    let groups = n.div_ceil(WORKGROUP).max(1);
    let x = groups.min(65535);
    (x, groups.div_ceil(x))
}

impl Gpu {
    fn buffer(&self, bytes: u64, extra: wgpu::BufferUsages) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes.max(16),
            usage: wgpu::BufferUsages::STORAGE | extra,
            mapped_at_creation: false,
        })
    }

    /// Runs `passes` (pipeline, bindings, grid) in order, in one submission.
    fn passes(&self, passes: &[Pass]) {
        let binds: Vec<_> = passes.iter().map(|(p, b, _)| self.bind(p, b)).collect();
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            for ((p, _, (x, y)), bind) in passes.iter().zip(&binds) {
                pass.set_pipeline(p);
                pass.set_bind_group(0, bind, &[]);
                pass.dispatch_workgroups(*x, *y, 1);
            }
        }
        self.queue.submit([enc.finish()]);
    }

    /// Reads a buffer of f32 back to the CPU (tests, samples).
    pub fn read_f32(&self, buf: &wgpu::Buffer) -> Vec<f32> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: buf.size(),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, buf.size());
        self.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("GPU readback failed"));
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU device lost");
        let out = bytemuck::cast_slice(&slice.get_mapped_range().expect("map GPU readback")).to_vec();
        staging.unmap();
        out
    }

    fn raw_params(&self, raw: &RawData, size: [u32; 4]) -> wgpu::Buffer {
        let g = raw.camera.as_shot;
        let (gx, gy) = grid(raw.width * raw.height);
        self.storage(bytemuck::bytes_of(&Params {
            width: raw.width,
            height: raw.height,
            pw: raw.pattern_size[0],
            ph: raw.pattern_size[1],
            groups: gx * gy,
            _pad: [0; 3],
            clip: [g[0], g[1], g[2], 0.0],
            crop: raw.crop,
            size,
            pattern: raw.pattern,
        }))
    }

    /// Develops `raw` up to camera RGB and its mip chain: highlights
    /// rebuilt, RCD demosaic (Bayer), cropped to the visible area.
    pub fn load_raw(&self, raw: &RawData) -> Result<GpuRaw, String> {
        let [_, _, cw, ch] = raw.crop;
        let n = raw.width * raw.height;
        let level0 = self.buffer(cw as u64 * ch as u64 * 8, wgpu::BufferUsages::COPY_SRC);
        let p = &self.raw;
        let params = self.raw_params(raw, [0; 4]);
        match &raw.pixels {
            Pixels::Bayer(mosaic) => {
                let cfa = self.reconstruct(raw, mosaic)?;
                let plane = || self.buffer(n as u64 * 4, wgpu::BufferUsages::COPY_SRC);
                let (r, g, b, dir, lpf) = (plane(), plane(), plane(), plane(), plane());
                let all = grid(n);
                self.passes(&[
                    (&p.rcd_init, &[(0, &params), (2, &cfa), (5, &r), (6, &g), (7, &b), (9, &lpf)], all),
                    (&p.rcd_vh, &[(0, &params), (2, &cfa), (8, &dir)], all),
                    (&p.rcd_green, &[(0, &params), (2, &cfa), (6, &g), (8, &dir), (9, &lpf)], all),
                    (&p.rcd_pq, &[(0, &params), (2, &cfa), (9, &lpf)], all),
                    (&p.rcd_rb, &[(0, &params), (5, &r), (6, &g), (7, &b), (9, &lpf)], all),
                    (&p.rcd_rb_green, &[(0, &params), (5, &r), (6, &g), (7, &b), (8, &dir)], all),
                    (&p.rcd_pack, &[(0, &params), (5, &r), (6, &g), (7, &b), (10, &level0)], grid(cw * ch)),
                ]);
            }
            Pixels::Rgb(px) => {
                let rgb = self.storage(bytemuck::cast_slice(px));
                self.passes(&[(&p.rgb_pack, &[(0, &params), (1, &rgb), (10, &level0)], grid(n))]);
            }
        }
        let mut levels = vec![(level0, cw, ch)];
        while let Some(&(ref src, w, h)) = levels.last().filter(|l| l.1.max(l.2) > MIP_MIN) {
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let dst = self.buffer(nw as u64 * nh as u64 * 8, wgpu::BufferUsages::COPY_SRC);
            let params = self.raw_params(raw, [w, h, nw, nh]);
            self.passes(&[(&p.mip, &[(0, &params), (10, src), (11, &dst)], grid(nw * nh))]);
            levels.push((dst, nw, nh));
        }
        Ok(GpuRaw { levels, camera: raw.camera.clone(), orientation: raw.orientation })
    }

    /// A mip level as f32 RGB (tests).
    #[cfg(test)]
    fn read_level(&self, raw: &GpuRaw, k: usize) -> Vec<[f32; 3]> {
        let words: Vec<u32> = bytemuck::cast_slice(&self.read_f32(&raw.levels[k].0)).to_vec();
        let half = |h: u32| f16_to_f32(h as u16);
        words.chunks_exact(2).map(|w| [half(w[0]), half(w[0] >> 16), half(w[1])]).collect()
    }

    /// The mosaic white-balanced as shot, with clipped highlights rebuilt.
    pub fn reconstruct(&self, raw: &RawData, mosaic: &[f32]) -> Result<wgpu::Buffer, String> {
        let n = raw.width * raw.height;
        let bytes = n as u64 * 4;
        let limits = self.device.limits();
        if bytes > limits.max_storage_buffer_binding_size || bytes > limits.max_buffer_size {
            return Err(format!("photo too large for this GPU ({} MP)", n / 1_000_000));
        }
        let (gx, gy) = grid(n);
        let params = self.raw_params(raw, [0; 4]);
        let cfa = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cfa"),
            contents: bytemuck::cast_slice(mosaic),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let out = self.buffer(bytes, wgpu::BufferUsages::COPY_SRC);
        let partials = self.buffer((gx * gy) as u64 * 6 * 4, wgpu::BufferUsages::empty());
        let chroma = self.buffer(16, wgpu::BufferUsages::empty());
        let p = &self.raw;
        self.passes(&[
            (&p.white_balance, &[(0, &params), (1, &cfa)], (gx, gy)),
            (&p.hl_partials, &[(0, &params), (1, &cfa), (3, &partials)], (gx, gy)),
            (&p.hl_reduce, &[(0, &params), (3, &partials), (4, &chroma)], (1, 1)),
            (&p.hl_apply, &[(0, &params), (1, &cfa), (2, &out), (4, &chroma)], (gx, gy)),
        ]);
        Ok(out)
    }
}

/// IEEE half → f32 (tests and CPU readback of mip levels).
#[cfg(test)]
pub(crate) fn f16_to_f32(h: u16) -> f32 {
    let (sign, exp, frac) = ((h >> 15) as u32, ((h >> 10) & 0x1f) as u32, (h & 0x3ff) as u32);
    let bits = match exp {
        0 if frac == 0 => sign << 31,
        0 => {
            // Subnormal: renormalise.
            let shift = frac.leading_zeros() - 21;
            (sign << 31) | ((113 - shift) << 23) | (((frac << shift) & 0x3ff) << 13)
        }
        31 => (sign << 31) | (0xff << 23) | (frac << 13),
        _ => (sign << 31) | ((exp + 112) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RGGB: [u32; 4] = [0, 1, 1, 2];

    fn data(w: u32, h: u32, as_shot: [f32; 3]) -> RawData {
        let mut pattern = [0; 36];
        pattern[..4].copy_from_slice(&RGGB);
        let mut camera = Camera::neutral();
        camera.as_shot = as_shot;
        RawData {
            pixels: Pixels::Bayer(Vec::new()),
            width: w,
            height: h,
            pattern,
            pattern_size: [2, 2],
            crop: [0, 0, w, h],
            orientation: 1,
            camera,
        }
    }

    fn colour(x: u32, y: u32) -> usize {
        RGGB[((y % 2) * 2 + x % 2) as usize] as usize
    }

    /// A mosaic sampling `f(x, y) -> rgb` (scene before white balance).
    fn mosaic(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> Vec<f32> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)[colour(x, y)]).collect()
    }

    #[test]
    fn f16_decodes() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
    }

    /// `mosaic` developed by `load_raw` (Bayer RGGB unless `pattern` says otherwise).
    fn develop(gpu: &Gpu, raw: &RawData, m: Vec<f32>) -> GpuRaw {
        let raw = RawData { pixels: Pixels::Bayer(m), camera: raw.camera.clone(), ..*raw };
        gpu.load_raw(&raw).unwrap()
    }

    fn rggb_at(phase: [u32; 4]) -> [u32; 36] {
        let mut p = [0; 36];
        p[..4].copy_from_slice(&phase);
        p
    }

    #[test]
    fn flat_field_is_exact() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let raw = data(48, 40, [1.0; 3]);
        let out = develop(&gpu, &raw, mosaic(48, 40, |_, _| [0.3, 0.5, 0.2]));
        for (i, c) in gpu.read_level(&out, 0).iter().enumerate() {
            assert!((0..3).all(|k| (c[k] - [0.3, 0.5, 0.2][k]).abs() < 1e-3), "{i}: {c:?}");
        }
    }

    #[test]
    fn every_bayer_phase_works() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        for phase in [[0, 1, 1, 2], [2, 1, 1, 0], [1, 0, 2, 1], [1, 2, 0, 1]] {
            let mut raw = data(32, 32, [1.0; 3]);
            raw.pattern = rggb_at(phase);
            let want = [0.25, 0.6, 0.15];
            let m = (0..32 * 32).map(|i| want[phase[((i / 32 % 2) * 2 + i % 2) as usize] as usize]).collect();
            for c in gpu.read_level(&develop(&gpu, &raw, m), 0) {
                assert!((0..3).all(|k| (c[k] - want[k]).abs() < 1e-3), "{phase:?}: {c:?}");
            }
        }
    }

    #[test]
    fn smooth_gradient_is_close() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (64, 64);
        let f = |x: u32, y: u32| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            [0.1 + 0.5 * u, 0.2 + 0.4 * v, 0.15 + 0.3 * u + 0.2 * v]
        };
        let out = gpu.read_level(&develop(&gpu, &data(w, h, [1.0; 3]), mosaic(w, h, f)), 0);
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                let (c, t) = (out[(y * w + x) as usize], f(x, y));
                assert!((0..3).all(|k| (c[k] - t[k]).abs() < 3e-3), "({x}, {y}): {c:?} vs {t:?}");
            }
        }
    }

    #[test]
    fn sharp_edge_has_no_zipper() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (48, 32);
        let out = gpu.read_level(
            &develop(&gpu, &data(w, h, [1.0; 3]), mosaic(w, h, |x, _| if x < 24 { [0.05; 3] } else { [0.8; 3] })),
            0,
        );
        for y in 8..h - 8 {
            for x in [20, 27] {
                let c = out[(y * w + x) as usize];
                assert!((c[1] - c[0]).abs() < 0.05 && (c[1] - c[2]).abs() < 0.05, "({x}, {y}): {c:?}");
            }
        }
    }

    /// Grey fine detail (vertical and diagonal waves near half Nyquist):
    /// RCD must clearly beat bilinear, which blurs it and adds false colour.
    #[test]
    fn rcd_beats_bilinear_on_fine_detail() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (96, 96);
        let f = |x: u32, y: u32| {
            let v = 0.4 + 0.25 * (x as f32 * 1.2).sin() + 0.15 * ((x + y) as f32 * 0.9).sin();
            [v; 3]
        };
        let m = mosaic(w, h, f);
        let bilinear = |x: u32, y: u32, c: usize| {
            let (mut s, mut k) = (0.0, 0.0);
            for yy in y - 1..=y + 1 {
                for xx in x - 1..=x + 1 {
                    if colour(xx, yy) == c {
                        (s, k) = (s + m[(yy * w + xx) as usize], k + 1.0);
                    }
                }
            }
            s / k
        };
        let out = gpu.read_level(&develop(&gpu, &data(w, h, [1.0; 3]), m.clone()), 0);
        let (mut rcd, mut bil) = (0.0, 0.0);
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                let t = f(x, y);
                for c in 0..3 {
                    rcd += (out[(y * w + x) as usize][c] - t[c]).abs();
                    bil += (bilinear(x, y, c) - t[c]).abs();
                }
            }
        }
        assert!(rcd < 0.7 * bil, "RCD error {rcd}, bilinear {bil}");
    }

    #[test]
    fn crop_keeps_the_visible_area() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let mut raw = data(40, 30, [1.0; 3]);
        raw.crop = [8, 6, 20, 14];
        let out = develop(&gpu, &raw, mosaic(40, 30, |x, _| if x < 18 { [0.2; 3] } else { [0.7; 3] }));
        assert_eq!(out.size(), (20, 14));
        let px = gpu.read_level(&out, 0);
        assert!((px[7 * 20 + 2][1] - 0.2).abs() < 1e-3 && (px[7 * 20 + 17][1] - 0.7).abs() < 1e-3);
    }

    #[test]
    fn mips_preserve_the_mean() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (600, 400);
        let out = develop(&gpu, &data(w, h, [1.0; 3]), mosaic(w, h, |x, y| [x as f32 / 600.0, y as f32 / 400.0, 0.3]));
        assert!(out.levels.len() >= 2 && out.levels.last().unwrap().1.max(out.levels.last().unwrap().2) <= MIP_MIN);
        let mean = |px: Vec<[f32; 3]>| {
            let n = px.len() as f32;
            px.iter().fold([0.0; 3], |a, c| std::array::from_fn(|k| a[k] + c[k] / n))
        };
        let m0 = mean(gpu.read_level(&out, 0));
        for k in 1..out.levels.len() {
            let mk = mean(gpu.read_level(&out, k));
            assert!((0..3).all(|c| (mk[c] - m0[c]).abs() < 1e-3), "level {k}: {mk:?} vs {m0:?}");
        }
    }

    /// Times a real file (RAWVALLEY_RAW): `cargo test --release -p rawvalley-core open_time -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn open_time() {
        let Ok(path) = std::env::var("RAWVALLEY_RAW") else { return eprintln!("no sample, skipped") };
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let t = std::time::Instant::now();
        let raw = read_raw(Path::new(&path)).unwrap();
        let read = t.elapsed();
        let out = gpu.load_raw(&raw).unwrap();
        gpu.read_f32(&out.levels.last().unwrap().0); // waits for the GPU
        eprintln!("read {read:?}, total {:?}, {:?}", t.elapsed(), out.size());
    }

    #[test]
    fn grid_stays_under_the_dispatch_limit() {
        let (x, y) = grid(24_000_000);
        assert!(x <= 65535 && x * y * WORKGROUP >= 24_000_000);
    }

    #[test]
    fn white_balance_scales_each_colour() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let raw = data(8, 8, [2.0, 1.0, 1.5]);
        let m = mosaic(8, 8, |_, _| [0.2, 0.3, 0.4]);
        let out = gpu.read_f32(&gpu.reconstruct(&raw, &m).unwrap());
        for (i, v) in out.iter().enumerate() {
            let want = [0.4, 0.3, 0.6][colour(i as u32 % 8, i as u32 / 8)];
            assert!((v - want).abs() < 1e-6, "{i}: {v} vs {want}");
        }
    }

    #[test]
    fn unclipped_pixels_are_untouched() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let raw = data(64, 48, [1.8, 1.0, 1.4]);
        let m = mosaic(64, 48, |x, y| {
            let t = ((x * 7 + y * 13) % 17) as f32 / 17.0;
            [0.85 * t, 0.85 * (1.0 - t), 0.6]
        });
        let out = gpu.read_f32(&gpu.reconstruct(&raw, &m).unwrap());
        let g = raw.camera.as_shot;
        for (i, v) in out.iter().enumerate() {
            let want = m[i] * g[colour(i as u32 % 64, i as u32 / 64)];
            assert_eq!(*v, want, "pixel {i}");
        }
    }

    /// A warm blob whose green clips: rebuilt green goes above the clip and
    /// its hue gets closer to the truth than the clipped data's.
    #[test]
    fn clipped_green_is_rebuilt_above_clip() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (96, 96);
        let raw = data(w, h, [1.0, 1.0, 1.0]);
        let light = |x: u32, y: u32| {
            let d = ((x as f32 - 48.0).powi(2) + (y as f32 - 48.0).powi(2)).sqrt();
            1.4 * (1.0 - d / 60.0).max(0.0)
        };
        let truth = |x, y| [0.6 * light(x, y), light(x, y), 0.4 * light(x, y)]; // G clips first
        let m = mosaic(w, h, |x, y| truth(x, y).map(|v| v.min(1.0)));
        let out = gpu.read_f32(&gpu.reconstruct(&raw, &m).unwrap());
        // Centre green photosite (48, 49 is G in RGGB: odd row, even column).
        let (x, y) = (48, 49);
        assert_eq!(colour(x, y), 1);
        let i = (y * w + x) as usize;
        assert!(out[i] > 1.0, "green not rebuilt: {}", out[i]);
        assert_eq!(colour(x - 1, y), 2);
        let b = out[i - 1]; // blue neighbour, unclipped
        let truth_ratio = 0.4;
        let (rebuilt, clipped) = (b / out[i], b / m[i]);
        assert!(
            (rebuilt - truth_ratio).abs() < 0.5 * (clipped - truth_ratio).abs(),
            "B/G rebuilt {rebuilt}, clipped {clipped}, truth {truth_ratio}"
        );
    }

    #[test]
    fn fully_clipped_area_stays_white() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (64, 64);
        let raw = data(w, h, [1.0, 1.0, 1.0]);
        let m = mosaic(w, h, |x, y| if (24..40).contains(&x) && (24..40).contains(&y) { [1.0; 3] } else { [0.5; 3] });
        let out = gpu.read_f32(&gpu.reconstruct(&raw, &m).unwrap());
        let v: Vec<f32> = (30..34).flat_map(|y| (30..34).map(move |x| (y * w + x) as usize)).map(|i| out[i]).collect();
        let (lo, hi) = v.iter().fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        assert!(lo >= 1.0 && hi / lo < 1.02, "{v:?}");
    }

    #[test]
    fn xtrans_pattern_is_honoured() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        const XT: [u32; 36] = [
            1, 1, 0, 1, 1, 2, 1, 1, 2, 1, 1, 0, 2, 0, 1, 0, 2, 1, 1, 1, 2, 1, 1, 0, 1, 1, 0, 1, 1, 2, 0, 2, 1, 2, 0, 1,
        ];
        let mut raw = data(36, 36, [1.7, 1.0, 1.3]);
        (raw.pattern, raw.pattern_size) = (XT, [6, 6]);
        let col = |x: u32, y: u32| XT[((y % 6) * 6 + x % 6) as usize] as usize;
        let m: Vec<f32> = (0..36 * 36).map(|i| [0.2, 0.3, 0.4][col(i % 36, i / 36)]).collect();
        let out = gpu.read_f32(&gpu.reconstruct(&raw, &m).unwrap());
        for (i, v) in out.iter().enumerate() {
            let c = col(i as u32 % 36, i as u32 / 36);
            assert_eq!(*v, m[i] * raw.camera.as_shot[c], "pixel {i}");
        }
    }
}
