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

/// Compute pipelines of `raw.wgsl`.
pub struct Pipelines {
    white_balance: wgpu::ComputePipeline,
    hl_partials: wgpu::ComputePipeline,
    hl_reduce: wgpu::ComputePipeline,
    hl_apply: wgpu::ComputePipeline,
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

    /// The mosaic white-balanced as shot, with clipped highlights rebuilt.
    pub fn reconstruct(&self, raw: &RawData, mosaic: &[f32]) -> Result<wgpu::Buffer, String> {
        let n = raw.width * raw.height;
        let bytes = n as u64 * 4;
        let limits = self.device.limits();
        if bytes > limits.max_storage_buffer_binding_size || bytes > limits.max_buffer_size {
            return Err(format!("photo too large for this GPU ({} MP)", n / 1_000_000));
        }
        let g = raw.camera.as_shot;
        let (gx, gy) = grid(n);
        let params = Params {
            width: raw.width,
            height: raw.height,
            pw: raw.pattern_size[0],
            ph: raw.pattern_size[1],
            groups: gx * gy,
            _pad: [0; 3],
            clip: [g[0], g[1], g[2], 0.0],
            pattern: raw.pattern,
        };
        let params = self.storage(bytemuck::bytes_of(&params));
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
