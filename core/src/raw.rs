//! RAW files on the GPU: the decoded mosaic is uploaded once, white-balanced
//! as shot and its clipped highlights rebuilt (`raw.wgsl`), then demosaiced.

use std::path::Path;

use rawler::imgop::chromatic_adaption::bradford_adaption_matrix;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse, transform_1d};
use rawler::imgop::sensor::SensorType;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawPhotometricInterpretation;
use wgpu::util::DeviceExt;

use crate::develop::{Camera, Develop};
use crate::gpu::{Gpu, GpuPhoto, WORKGROUP};
use crate::photo::{Photo, RAW_TO_SCENE};
use crate::profiles::{Profile, rec2020_to_rec709};
use crate::view::Region;

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
    let (mut raw, orientation) = decode(path)?;
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
            camera: camera(&raw),
        });
    }
    let (rgb, w, h, camera) = cpu_rgb(&raw)?;
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

/// A RAW file decoded by rawler, and its EXIF orientation (1–8).
pub(crate) fn decode(path: &Path) -> Result<(rawler::RawImage, u16), String> {
    let src = rawler::rawsource::RawSource::new(path).map_err(|e| e.to_string())?;
    let params = rawler::decoders::RawDecodeParams::default();
    let raw = rawler::decode(&src, &params).map_err(|e| e.to_string())?;
    // Decoders such as Panasonic's leave `raw.orientation` at Normal; the EXIF
    // Orientation tag is the reliable source (6/8 = portrait shots).
    let orientation = rawler::get_decoder(&src)
        .and_then(|d| d.raw_metadata(&src, &params))
        .ok()
        .and_then(|m| m.exif.orientation)
        .unwrap_or_else(|| raw.orientation.to_u16());
    Ok((raw, orientation))
}

/// Camera RGB pixels, width, height and camera model.
pub(crate) type CpuRgb = (Vec<[f32; 3]>, usize, usize, Camera);

/// Demosaiced by rawler on the CPU (X-Trans, 4-colour and linear RAWs, small
/// copies): as-shot-balanced camera RGB, its size, and the camera model.
pub(crate) fn cpu_rgb(raw: &rawler::RawImage) -> Result<CpuRgb, String> {
    use ProcessingStep::*;
    let dev = RawDevelop::new_with(&[Rescale, Demosaic, FujiRotate, CropActiveArea, CropDefault]);
    match dev.develop_intermediate(raw).map_err(|e| e.to_string())? {
        Intermediate::ThreeColor(mut px) => {
            let camera = camera(raw);
            let g = camera.as_shot;
            px.data.iter_mut().for_each(|p| (0..3).for_each(|c| p[c] *= g[c]));
            Ok((px.data, px.width, px.height, camera))
        }
        // shortcut: CYGM/RGBE sensors are calibrated to Rec.2020 here, so Kelvin WB acts on Rec.2020; upgrade if one is reported.
        Intermediate::FourColor(px) => {
            let cal = four_colour(raw);
            Ok((px.data.iter().map(cal).collect(), px.width, px.height, Camera::neutral()))
        }
        Intermediate::Monochrome(_) => Err("unsupported sensor layout".into()),
    }
}

/// D65-adapted XYZ → camera matrix of an N-colour sensor, when the file has one.
fn xyz2cam<const N: usize>(raw: &rawler::RawImage) -> Option<[[f32; 3]; N]> {
    use Illuminant::*;
    let (illu, m) = raw.color_matrix_find_first([D65, A, B, C, D50, D55, D75, Daylight, Flash])?;
    let m = transform_1d::<N, 3>(m.get(..N * 3)?)?;
    Some(if illu == D65 { m } else { multiply(&m, &bradford_adaption_matrix(&illu, &D65)) })
}

/// The camera's colour model from the file: D65-adapted colour matrix and
/// as-shot multipliers.
pub(crate) fn camera(raw: &rawler::RawImage) -> Camera {
    let xyz2cam = xyz2cam::<3>(raw);
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

    /// Full-resolution size as shown (after orientation).
    pub fn oriented_size(&self) -> (u32, u32) {
        crate::photo::oriented_size(self.size(), self.orientation)
    }
}

/// Longest side of the smallest mip level.
const MIP_MIN: u32 = 256;

/// CYGM/RGBE camera values → linear Rec.2020: as-shot WB and the 4×3 colour
/// matrix, as phase 1 calibrated them (to Rec.709 then).
pub(crate) fn four_colour(raw: &rawler::RawImage) -> impl Fn(&[f32; 4]) -> [f32; 3] + Sync + use<> {
    let wb: [f32; 4] =
        std::array::from_fn(|c| if raw.wb_coeffs[0].is_nan() { 1.0 } else { raw.wb_coeffs[c] / raw.wb_coeffs[1] });
    let xyz2cam = xyz2cam::<4>(raw);
    let cam2rgb: Option<[[f32; 4]; 3]> =
        xyz2cam.map(|m| pseudo_inverse(normalize(multiply(&m, &crate::profiles::rec2020_to_xyz()))));
    move |p| {
        let cam: [f32; 4] = std::array::from_fn(|c| p[c] * wb[c]);
        match &cam2rgb {
            Some(m) => m.map(|row| row.iter().zip(&cam).map(|(a, v)| a * v).sum()),
            // No matrix: the two greens averaged, the rest as is.
            None => [cam[0], 0.5 * (cam[1] + cam[3]), cam[2]],
        }
    }
}

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
    develop: wgpu::ComputePipeline,
    /// The OETF table of the last profile developed into.
    oetf: std::cell::RefCell<Option<(Profile, wgpu::Buffer)>>,
    /// `DevelopParams`, rewritten per frame.
    develop_params: wgpu::Buffer,
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
            develop: pipeline("develop"),
            oetf: Default::default(),
            develop_params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("develop params"),
                size: std::mem::size_of::<DevelopParams>() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
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

/// `DevelopParams` of `raw.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DevelopParams {
    sizes: [u32; 8],
    region: [f32; 4],
    rotation: [f32; 4],
    gains: [f32; 4],
    cam: [[f32; 4]; 3],
    target: [[f32; 4]; 3],
    k: f32,
    _pad: [f32; 3],
}

/// Rows of a row-major 3×3 matrix, padded as WGSL lays out `mat3x3` columns
/// (the shader multiplies `v * m`, i.e. by the rows).
fn rows(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    m.map(|r| [r[0], r[1], r[2], 0.0])
}

/// Samples of a profile's OETF the develop pass interpolates: 256 linear
/// steps over [−1/16, 1/16), then log2 steps from 1/16 to 256.
fn oetf_table(profile: Profile) -> Vec<f32> {
    const LINEAR: usize = 256;
    const N: usize = 4096;
    (0..N)
        .map(|i| {
            let v = if i < LINEAR {
                -1.0 / 16.0 + i as f32 / (LINEAR - 1) as f32 / 8.0
            } else {
                ((i - LINEAR) as f32 / (N - LINEAR - 1) as f32 * 12.0 - 4.0).exp2()
            };
            profile.encode(v)
        })
        .collect()
}

/// Workgroups covering `n` items on a 2-D grid (≤ 65535 per dimension).
fn grid(n: u32) -> (u32, u32) {
    let groups = n.div_ceil(WORKGROUP).max(1);
    let x = groups.min(65535);
    (x, groups.div_ceil(x))
}

impl Gpu {
    pub(crate) fn buffer(&self, bytes: u64, extra: wgpu::BufferUsages) -> wgpu::Buffer {
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
        bytemuck::cast_slice(&self.read_range(buf, 0, buf.size())).to_vec()
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

    /// Refuses a buffer this GPU cannot bind (wgpu would panic on it).
    pub(crate) fn fits(&self, bytes: u64) -> Result<(), String> {
        let limits = self.device.limits();
        if bytes > limits.max_storage_buffer_binding_size || bytes > limits.max_buffer_size {
            return Err(format!("photo too large for this GPU ({} MB buffer)", bytes / 1_000_000));
        }
        Ok(())
    }

    /// Develops `raw` up to camera RGB and its mip chain: highlights
    /// rebuilt, RCD demosaic (Bayer), cropped to the visible area.
    pub fn load_raw(&self, raw: &RawData) -> Result<GpuRaw, String> {
        let [_, _, cw, ch] = raw.crop;
        let n = raw.width * raw.height;
        // The largest buffers: level 0 (f16 RGBA), and the RGB upload or the CFA planes.
        self.fits(cw as u64 * ch as u64 * 8)?;
        self.fits(n as u64 * if matches!(raw.pixels, Pixels::Rgb(_)) { 12 } else { 4 })?;
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

    /// `r` (oriented full-resolution coordinates) of `raw` developed to
    /// `w` × `h` as `profile`'s signal, ready for the grade shader.
    pub fn develop(
        &self,
        raw: &GpuRaw,
        r: &Region,
        w: u32,
        h: u32,
        dev: &Develop,
        profile: Profile,
    ) -> Result<GpuPhoto, String> {
        let mut slot = None;
        self.develop_into(&mut slot, raw, r, w, h, dev, profile)?;
        Ok(slot.expect("developed"))
    }

    /// [`Gpu::develop`] into `slot`, reusing its buffers when the size matches
    /// (a drag allocates nothing per frame).
    #[allow(clippy::too_many_arguments)]
    pub fn develop_into(
        &self,
        slot: &mut Option<GpuPhoto>,
        raw: &GpuRaw,
        r: &Region,
        w: u32,
        h: u32,
        dev: &Develop,
        profile: Profile,
    ) -> Result<(), String> {
        self.fits(w as u64 * h as u64 * 16)?;
        // The finest level at least as coarse as one output pixel, as `Pyramid::resample` picks.
        let (sx, sy) = (r.size[0] / w as f32, r.size[1] / h as f32);
        let scale = sx.max(sy);
        let (sin, cos) = r.angle.to_radians().sin_cos();
        let k = (scale.max(1.0).log2().floor() as usize).min(raw.levels.len() - 1);
        let ((level, lw, lh), (fw, fh)) = (&raw.levels[k], raw.size());
        let target = if profile.is_log() { profile.from_rec2020() } else { rec2020_to_rec709() };
        let params = DevelopParams {
            sizes: [w, h, *lw, *lh, fw, fh, raw.orientation as u32, profile.is_log() as u32],
            region: [r.center[0], r.center[1], sx, sy],
            rotation: [sin, cos, 0.0, 0.0],
            gains: {
                let g = raw.camera.gains_for(dev);
                [g[0], g[1], g[2], 0.0]
            },
            cam: rows(raw.camera.cam2rec2020),
            target: rows(target),
            k: RAW_TO_SCENE * dev.exposure.exp2(),
            _pad: [0.0; 3],
        };
        self.queue.write_buffer(&self.raw.develop_params, 0, bytemuck::bytes_of(&params));
        let mut oetf = self.raw.oetf.borrow_mut();
        if oetf.as_ref().is_none_or(|(p, _)| *p != profile) {
            *oetf = Some((profile, self.storage(bytemuck::cast_slice(&oetf_table(profile)))));
        }
        let table = &oetf.as_ref().expect("set above").1;
        let src = &self.photo_slot(slot, w, h).src;
        let p = &self.raw;
        self.passes(&[(&p.develop, &[(10, level), (12, &p.develop_params), (13, src), (14, table)], grid(w * h))]);
        Ok(())
    }

    /// The developed pixels back on the CPU.
    pub fn read_photo(&self, p: &GpuPhoto) -> Photo {
        let px = self.read_f32(&p.src);
        Photo {
            width: p.width,
            height: p.height,
            pixels: px.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        }
    }

    /// The largest mip level whose longer side is at most `max`, as oriented
    /// camera RGB on the CPU (filmstrip copy, match source).
    pub fn small(&self, raw: &GpuRaw, max: u32) -> Photo {
        let k = raw.levels.iter().position(|l| l.1.max(l.2) <= max).unwrap_or(raw.levels.len() - 1);
        let (_, width, height) = raw.levels[k];
        let pixels = self.read_level(raw, k).into_iter().map(|[r, g, b]| [r, g, b, 1.0]).collect();
        Photo { width, height, pixels }.oriented(raw.orientation)
    }

    /// Mean camera RGB (as-shot balanced) of the full-resolution pixels within
    /// `radius` of `at`, a point of the oriented image (white-balance picker).
    pub fn sample(&self, raw: &GpuRaw, at: [f32; 2], radius: u32) -> [f32; 3] {
        let (w, h) = raw.size();
        let [x, y] = sensor_point(at, raw.orientation, w, h).map(|v| v.max(0.0) as u32);
        let (x, y) = (x.min(w - 1), y.min(h - 1));
        let (y0, y1) = (y.saturating_sub(radius), (y + radius).min(h - 1));
        let (x0, x1) = (x.saturating_sub(radius), (x + radius).min(w - 1));
        // Whole rows: one contiguous copy.
        let words = self.read_range(&raw.levels[0].0, y0 as u64 * w as u64 * 8, (y1 - y0 + 1) as u64 * w as u64 * 8);
        let words: &[u32] = bytemuck::cast_slice(&words);
        let mut sum = [0.0; 3];
        for yy in 0..=(y1 - y0) {
            for xx in x0..=x1 {
                let i = (yy * w + xx) as usize * 2;
                let c =
                    [f16_to_f32(words[i] as u16), f16_to_f32((words[i] >> 16) as u16), f16_to_f32(words[i + 1] as u16)];
                (0..3).for_each(|k| sum[k] += c[k]);
            }
        }
        let n = ((y1 - y0 + 1) * (x1 - x0 + 1)) as f32;
        sum.map(|v| v / n)
    }

    /// `len` bytes of `buf` from `offset`, back on the CPU.
    fn read_range(&self, buf: &wgpu::Buffer, offset: u64, len: u64) -> Vec<u8> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(buf, offset, &staging, 0, len);
        self.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("GPU readback failed"));
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU device lost");
        let out = slice.get_mapped_range().expect("map GPU readback").to_vec();
        staging.unmap();
        out
    }

    /// A mip level as f32 RGB.
    fn read_level(&self, raw: &GpuRaw, k: usize) -> Vec<[f32; 3]> {
        let words: Vec<u32> = bytemuck::cast_slice(&self.read_f32(&raw.levels[k].0)).to_vec();
        words.chunks_exact(2).map(unpack).collect()
    }

    /// The mosaic white-balanced as shot, with clipped highlights rebuilt.
    pub fn reconstruct(&self, raw: &RawData, mosaic: &[f32]) -> Result<wgpu::Buffer, String> {
        let n = raw.width * raw.height;
        let bytes = n as u64 * 4;
        self.fits(bytes)?;
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

/// Sensor (unoriented) position of a point of the oriented image; `w` × `h`
/// is the sensor size. Same mapping as `sensor` in `raw.wgsl`.
fn sensor_point([x, y]: [f32; 2], orientation: u16, w: u32, h: u32) -> [f32; 2] {
    let (w, h) = (w as f32, h as f32);
    match orientation {
        2 => [w - x, y],
        3 => [w - x, h - y],
        4 => [x, h - y],
        5 => [y, x],
        6 => [y, h - x],
        7 => [w - y, h - x],
        8 => [w - y, x],
        _ => [x, y],
    }
}

/// A packed level pixel (two words of f16, as `pack` in `raw.wgsl` writes them) as RGB.
fn unpack(w: &[u32]) -> [f32; 3] {
    [f16_to_f32(w[0] as u16), f16_to_f32((w[0] >> 16) as u16), f16_to_f32(w[1] as u16)]
}

/// IEEE half → f32 (CPU readback of mip levels).
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

    /// Camera RGB `px` (w × h) uploaded as a demosaiced RAW.
    fn rgb_raw(gpu: &Gpu, w: u32, h: u32, px: Vec<[f32; 3]>, orientation: u16, camera: Camera) -> GpuRaw {
        let raw = RawData {
            pixels: Pixels::Rgb(px),
            width: w,
            height: h,
            pattern: [0; 36],
            pattern_size: [1, 1],
            crop: [0, 0, w, h],
            orientation,
            camera,
        };
        gpu.load_raw(&raw).unwrap()
    }

    fn whole(w: u32, h: u32) -> Region {
        Region { center: [w as f32 / 2.0, h as f32 / 2.0], size: [w as f32, h as f32], angle: 0.0 }
    }

    #[test]
    fn develop_matches_cpu_reference() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let xyz2cam = [[0.7, -0.15, -0.06], [-0.42, 1.24, 0.19], [-0.08, 0.17, 0.62]];
        let camera = Camera::new(xyz2cam, [2.0, 1.0, 1.6], 0.0);
        let patches = [[0.02, 0.03, 0.025], [0.2, 0.3, 0.25], [0.5, 0.4, 0.2], [0.05, 0.3, 0.6], [0.9, 0.95, 0.9]];
        let raw = rgb_raw(&gpu, 5, 1, patches.to_vec(), 1, camera.clone());
        for profile in [Profile::Standard, Profile::VLog, Profile::SLog3, Profile::LogC4] {
            for dev in [Develop::default(), Develop { wb: Some([3200.0, 10.0]), exposure: 0.7 }] {
                let out = gpu.read_photo(&gpu.develop(&raw, &whole(5, 1), 5, 1, &dev, profile).unwrap());
                for (c, got) in patches.iter().zip(&out.pixels) {
                    let want = crate::develop::develop_pixel(*c, &camera, &dev, profile);
                    // f16 storage of the camera RGB, then the OETF table.
                    assert!(
                        (0..3).all(|k| (got[k] - want[k]).abs() < 2e-3),
                        "{profile:?} {dev:?} {c:?}: {got:?} vs {want:?}"
                    );
                }
            }
        }
    }

    /// Every EXIF orientation gives what the image crate's `apply_orientation` does.
    #[test]
    fn develop_honours_orientation() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (4, 2);
        let px: Vec<[f32; 3]> = (0..w * h).map(|i| [i as f32 / 8.0, 0.1, 0.1]).collect();
        for o in 1..=8u16 {
            let raw = rgb_raw(&gpu, w, h, px.clone(), o, Camera::neutral());
            let (ow, oh) = if o >= 5 { (h, w) } else { (w, h) };
            let dev = Develop::default();
            let got = gpu.read_photo(&gpu.develop(&raw, &whole(ow, oh), ow, oh, &dev, Profile::VLog).unwrap());
            let mut img = image::DynamicImage::ImageRgba32F(image::Rgba32FImage::from_fn(w, h, |x, y| {
                image::Rgba([px[(y * w + x) as usize][0], 0.1, 0.1, 1.0])
            }));
            img.apply_orientation(image::metadata::Orientation::from_exif(o as u8).unwrap());
            let want = img.into_rgba32f();
            assert_eq!((want.width(), want.height()), (ow, oh));
            for (i, p) in want.pixels().enumerate() {
                let enc = crate::develop::develop_pixel([p[0], 0.1, 0.1], &Camera::neutral(), &dev, Profile::VLog);
                assert!((got.pixels[i][0] - enc[0]).abs() < 2e-3, "orientation {o}, pixel {i}");
            }
        }
    }

    /// On a real file (RAWVALLEY_RAW) the GPU develop and the CPU small copy
    /// (rawler's demosaic) agree on the fitted photo: same framing,
    /// orientation, crop and colour. Also prints the develop time.
    #[test]
    fn gpu_and_cpu_raw_paths_agree() {
        let Ok(path) = std::env::var("RAWVALLEY_RAW") else { return eprintln!("no sample, skipped") };
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let raw = gpu.load_raw(&read_raw(Path::new(&path)).unwrap()).unwrap();
        let cpu = crate::photo::Decoded::load_max(Path::new(&path), Some(512)).unwrap();
        let (w, h) = cpu.size();
        let (fw, fh) = raw.size();
        assert_eq!((w, h), if raw.orientation >= 5 { (fh, fw) } else { (fw, fh) });
        let r = whole(w, h);
        let (ow, oh) = crate::photo::Decoded::fit_size(&r, 400);
        let dev = Develop::default();
        let t = std::time::Instant::now();
        let ph = gpu.develop(&raw, &r, ow, oh, &dev, Profile::Standard).unwrap();
        let a = gpu.read_photo(&ph);
        eprintln!("develop {ow}×{oh}: {:?}", t.elapsed());
        let b = cpu.encode_region(&r, ow, oh, Profile::Standard, &dev);
        let n = a.pixels.len() as f32;
        let rmse = (a
            .pixels
            .iter()
            .zip(&b.pixels)
            .map(|(p, q)| (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f32>())
            .sum::<f32>()
            / (3.0 * n))
            .sqrt();
        eprintln!("RMSE GPU vs CPU: {rmse}");
        // Preview frame cost (develop + readback) at Best, Balanced and Fast drafts of a 2048 px fit.
        let (pw, ph) = crate::photo::Decoded::fit_size(&r, 2048);
        for k in [1.0, 0.75, 0.5] {
            let (w, h) = ((pw as f32 * k) as u32, (ph as f32 * k) as u32);
            // As the worker does it: the frame stays on the GPU, a 512 px copy comes back.
            let (sw, sh) = crate::photo::Decoded::fit_size(&r, 512);
            let (mut slot, mut small, t) = (None, None, std::time::Instant::now());
            for _ in 0..10 {
                gpu.develop_into(&mut slot, &raw, &r, w, h, &dev, Profile::Standard).unwrap();
                gpu.develop_into(&mut small, &raw, &r, sw, sh, &dev, Profile::Standard).unwrap();
                gpu.read_photo(small.as_ref().unwrap());
            }
            eprintln!("frame {w}×{h}: {:?}", t.elapsed() / 10);
        }
        assert!(rmse < 0.03, "RMSE {rmse}");
    }

    /// The picker reads the pixel shown at a point, whatever the orientation.
    #[test]
    fn sample_follows_orientation() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (6, 4);
        let px: Vec<[f32; 3]> = (0..w * h).map(|i| [i as f32 / 32.0, 0.5, 0.25]).collect();
        for o in 1..=8u16 {
            let raw = rgb_raw(&gpu, w, h, px.clone(), o, Camera::neutral());
            let (ow, oh) = if o >= 5 { (h, w) } else { (w, h) };
            let shown =
                gpu.read_photo(&gpu.develop(&raw, &whole(ow, oh), ow, oh, &Develop::default(), Profile::VLog).unwrap());
            for (x, y) in [(0, 0), (ow - 1, 0), (1, oh - 1)] {
                let s = gpu.sample(&raw, [x as f32 + 0.5, y as f32 + 0.5], 0);
                let enc = crate::develop::develop_pixel(s, &Camera::neutral(), &Develop::default(), Profile::VLog);
                let got = shown.pixels[(y * ow + x) as usize];
                assert!((got[0] - enc[0]).abs() < 2e-3, "orientation {o} at ({x}, {y})");
            }
        }
    }

    /// A second develop of the same size writes into the same buffers.
    #[test]
    fn develop_reuses_buffers_of_the_same_size() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let raw = rgb_raw(&gpu, 4, 2, vec![[0.2; 3]; 8], 1, Camera::neutral());
        let mut slot = None;
        let dev = Develop::default();
        gpu.develop_into(&mut slot, &raw, &whole(4, 2), 4, 2, &dev, Profile::Standard).unwrap();
        let first = slot.as_ref().unwrap().src.clone();
        let bright = Develop { exposure: 1.0, ..dev };
        gpu.develop_into(&mut slot, &raw, &whole(4, 2), 4, 2, &bright, Profile::Standard).unwrap();
        assert!(slot.as_ref().unwrap().src == first, "same size, new buffer");
        let want = gpu.read_photo(&gpu.develop(&raw, &whole(4, 2), 4, 2, &bright, Profile::Standard).unwrap());
        assert_eq!(gpu.read_photo(slot.as_ref().unwrap()).pixels, want.pixels);
        gpu.develop_into(&mut slot, &raw, &whole(4, 2), 2, 1, &dev, Profile::Standard).unwrap();
        assert_eq!((slot.as_ref().unwrap().width, slot.as_ref().unwrap().height), (2, 1));
    }

    /// Buffers past the GPU's limits are refused with an error, not a wgpu panic.
    #[test]
    fn oversized_buffers_are_refused() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let max = gpu.device.limits().max_storage_buffer_binding_size;
        assert!(gpu.fits(max).is_ok() && gpu.fits(max + 1).is_err());
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
