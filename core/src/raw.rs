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

use crate::develop::{Camera, Detail, Develop};
use crate::gpu::{Gpu, GpuPhoto, WORKGROUP};
use crate::noise::NoiseModel;
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
    /// The same chain denoised by the AI (`Detail::nr_ai`), once it ran.
    pub(crate) ai: Option<Vec<(wgpu::Buffer, u32, u32)>>,
    pub camera: Camera,
    /// Noise of the full-resolution camera RGB, estimated at load.
    pub noise: NoiseModel,
    /// Unique per load: what the slots' stage caches were made from.
    pub(crate) id: u64,
    pub orientation: u16,
}

impl GpuRaw {
    /// Full-resolution size, before orientation.
    pub fn size(&self) -> (u32, u32) {
        (self.levels[0].1, self.levels[0].2)
    }

    /// `d` asks for the AI and it ran: its chain blends in, classic luminance NR is off.
    fn ai_active(&self, d: &Detail) -> bool {
        d.nr_ai && self.ai.is_some()
    }

    /// Level `k` of the AI chain, or of the classic one before the AI ran.
    fn ai_level(&self, k: usize) -> &wgpu::Buffer {
        &self.ai.as_ref().unwrap_or(&self.levels)[k].0
    }

    /// Full-resolution size as shown (after orientation).
    pub fn oriented_size(&self) -> (u32, u32) {
        crate::photo::oriented_size(self.size(), self.orientation)
    }
}

/// What a cached stage result was made from: the photo, then every
/// parameter upstream of it.
type Key = (u64, Vec<f32>);

/// A slot's linear working buffers: the develop with a margin all round for
/// the detail tools, scratch space, and the slow stages' last results.
pub(crate) struct Work {
    size: (u32, u32),
    margin: u32,
    lin: wgpu::Buffer,
    scratch: Option<[wgpu::Buffer; 2]>,
    after_nr: Option<(Key, wgpu::Buffer)>,
    after_tone: Option<(Key, wgpu::Buffer)>,
}

impl Work {
    fn scratch(&mut self, gpu: &Gpu) -> [&wgpu::Buffer; 2] {
        let bytes = self.lin.size();
        let [a, b] = self.scratch.get_or_insert_with(|| [0, 1].map(|_| gpu.buffer(bytes, wgpu::BufferUsages::empty())));
        [&*a, &*b]
    }
}

/// Keeps `lin` in `cache` as the result made from `key`.
fn keep(gpu: &Gpu, lin: &wgpu::Buffer, cache: &mut Option<(Key, wgpu::Buffer)>, key: Key) {
    let usage = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    let buf = cache.take().map_or_else(|| gpu.buffer(lin.size(), usage), |(_, b)| b);
    gpu.copy(lin, &buf);
    *cache = Some((key, buf));
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
    #[cfg(feature = "ai")]
    pub(crate) ai_pack: wgpu::ComputePipeline,
    #[cfg(feature = "ai")]
    pub(crate) ai_gain: wgpu::ComputePipeline,
    #[cfg(feature = "ai")]
    pub(crate) ai_unpack: wgpu::ComputePipeline,
    develop_linear: wgpu::ComputePipeline,
    develop_encode: wgpu::ComputePipeline,
    nr_gat: wgpu::ComputePipeline,
    nr_nlm: wgpu::ComputePipeline,
    nr_luma_apply: wgpu::ComputePipeline,
    nr_chroma_init: wgpu::ComputePipeline,
    nr_atrous: wgpu::ComputePipeline,
    nr_chroma_apply: wgpu::ComputePipeline,
    sh_init: wgpu::ComputePipeline,
    sh_ratio: wgpu::ComputePipeline,
    sh_update: wgpu::ComputePipeline,
    sh_apply: wgpu::ComputePipeline,
    us_init: wgpu::ComputePipeline,
    us_blur_h: wgpu::ComputePipeline,
    us_blur_v: wgpu::ComputePipeline,
    us_apply: wgpu::ComputePipeline,
    fr_blur_h: wgpu::ComputePipeline,
    fr_blur_v: wgpu::ComputePipeline,
    fr_manifolds: wgpu::ComputePipeline,
    fr_normalise: wgpu::ComputePipeline,
    fr_out: wgpu::ComputePipeline,
    fr_apply: wgpu::ComputePipeline,
    box_h: wgpu::ComputePipeline,
    box_v: wgpu::ComputePipeline,
    gf_ab: wgpu::ComputePipeline,
    dh_min_h: wgpu::ComputePipeline,
    dh_min_v: wgpu::ComputePipeline,
    dh_prep: wgpu::ComputePipeline,
    dh_apply: wgpu::ComputePipeline,
    ll_prep: wgpu::ComputePipeline,
    ll_reduce_g: wgpu::ComputePipeline,
    ll_reduce_q: wgpu::ComputePipeline,
    ll_top: wgpu::ComputePipeline,
    ll_collapse: wgpu::ComputePipeline,
    ll_apply: wgpu::ComputePipeline,
    /// Dehaze airlight (one f32 as bits, atomic max), zeroed per run.
    air: wgpu::Buffer,
    /// Noise reductions run (tests check the cache).
    #[cfg(test)]
    pub(crate) nr_runs: std::cell::Cell<u32>,
    /// The OETF table of the last profile developed into.
    oetf: std::cell::RefCell<Option<(Profile, wgpu::Buffer)>>,
    /// `DevelopParams`, rewritten per frame.
    develop_params: wgpu::Buffer,
    /// Open while a develop records: its passes and copies go out in one submission.
    pub(crate) batch: std::cell::RefCell<Option<wgpu::CommandEncoder>>,
    /// What the airlight in `air` was measured from (photo, gains, exposure, lens, AI blend).
    air_key: std::cell::RefCell<Option<(u64, [f32; 6])>>,
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let pipeline =
            crate::gpu::shader(device, "raw.wgsl", concat!(include_str!("common.wgsl"), include_str!("raw.wgsl")));
        let denoise = crate::gpu::shader(
            device,
            "denoise.wgsl",
            concat!(include_str!("common.wgsl"), include_str!("denoise.wgsl")),
        );
        let detail = crate::gpu::shader(
            device,
            "detail.wgsl",
            concat!(include_str!("common.wgsl"), include_str!("detail.wgsl")),
        );
        let fringe = crate::gpu::shader(
            device,
            "fringe.wgsl",
            concat!(include_str!("common.wgsl"), include_str!("fringe.wgsl")),
        );
        let tone =
            crate::gpu::shader(device, "tone.wgsl", concat!(include_str!("common.wgsl"), include_str!("tone.wgsl")));
        let ll = crate::gpu::shader(device, "ll.wgsl", concat!(include_str!("common.wgsl"), include_str!("ll.wgsl")));
        Self {
            box_h: tone("box_h"),
            box_v: tone("box_v"),
            gf_ab: tone("gf_ab"),
            dh_min_h: tone("dh_min_h"),
            dh_min_v: tone("dh_min_v"),
            dh_prep: tone("dh_prep"),
            dh_apply: tone("dh_apply"),
            ll_prep: ll("ll_prep"),
            ll_reduce_g: ll("ll_reduce_g"),
            ll_reduce_q: ll("ll_reduce_q"),
            ll_top: ll("ll_top"),
            ll_collapse: ll("ll_collapse"),
            ll_apply: ll("ll_apply"),
            #[cfg(test)]
            nr_runs: Default::default(),
            air: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("airlight"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            sh_init: detail("sh_init"),
            sh_ratio: detail("sh_ratio"),
            sh_update: detail("sh_update"),
            sh_apply: detail("sh_apply"),
            us_init: detail("us_init"),
            us_blur_h: detail("us_blur_h"),
            us_blur_v: detail("us_blur_v"),
            us_apply: detail("us_apply"),
            fr_blur_h: fringe("fr_blur_h"),
            fr_blur_v: fringe("fr_blur_v"),
            fr_manifolds: fringe("fr_manifolds"),
            fr_normalise: fringe("fr_normalise"),
            fr_out: fringe("fr_out"),
            fr_apply: fringe("fr_apply"),
            nr_gat: denoise("nr_gat"),
            nr_nlm: denoise("nr_nlm"),
            nr_luma_apply: denoise("nr_luma_apply"),
            nr_chroma_init: denoise("nr_chroma_init"),
            nr_atrous: denoise("nr_atrous"),
            nr_chroma_apply: denoise("nr_chroma_apply"),
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
            #[cfg(feature = "ai")]
            ai_pack: pipeline("ai_pack"),
            #[cfg(feature = "ai")]
            ai_gain: pipeline("ai_gain"),
            #[cfg(feature = "ai")]
            ai_unpack: pipeline("ai_unpack"),
            develop_linear: pipeline("develop_linear"),
            develop_encode: pipeline("develop_encode"),
            oetf: Default::default(),
            batch: Default::default(),
            air_key: Default::default(),
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
pub(crate) type Pass<'a> = (&'a wgpu::ComputePipeline, &'a [(u32, &'a wgpu::Buffer)], (u32, u32));

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
    lens: f32,
    margin: u32,
    lin_w: u32,
    /// Weight of the AI chain over the classic one.
    ai: f32,
    _pad: [u32; 3],
}

/// `DevelopParams` of `r` at `w` × `h` with `margin`, and the mip level it samples:
/// the finest level at least as coarse as one output pixel, as `Pyramid::resample` picks.
#[allow(clippy::too_many_arguments)]
fn develop_params(
    raw: &GpuRaw,
    r: &Region,
    w: u32,
    h: u32,
    dev: &Develop,
    profile: Profile,
    margin: u32,
) -> (DevelopParams, usize) {
    let (sx, sy) = (r.size[0] / w as f32, r.size[1] / h as f32);
    let (sin, cos) = r.angle.to_radians().sin_cos();
    let k = (sx.max(sy).max(1.0).log2().floor() as usize).min(raw.levels.len() - 1);
    let ((_, kw, kh), (fw, fh)) = (&raw.levels[k], raw.size());
    let target = if profile.is_log() { profile.from_rec2020() } else { rec2020_to_rec709() };
    let gains = raw.camera.gains_for(dev);
    let params = DevelopParams {
        sizes: [w, h, *kw, *kh, fw, fh, raw.orientation as u32, profile.is_log() as u32],
        region: [r.center[0], r.center[1], sx, sy],
        rotation: [sin, cos, 0.0, 0.0],
        gains: [gains[0], gains[1], gains[2], 0.0],
        cam: rows(raw.camera.cam2rec2020),
        target: rows(target),
        k: RAW_TO_SCENE * dev.exposure.exp2(),
        lens: dev.detail.lens_vignette / 100.0,
        margin,
        lin_w: w + 2 * margin,
        ai: if raw.ai_active(&dev.detail) { dev.detail.nr_ai_strength / 100.0 } else { 0.0 },
        _pad: [0; 3],
    };
    (params, k)
}

fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// `NrParams` of `denoise.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NrParams {
    size: [u32; 2],
    patch: i32,
    search: i32,
    step: i32,
    _pad: u32,
    a: f32,
    b: f32,
    luma: f32,
    detail: f32,
    chroma: f32,
    _pad2: f32,
}

/// `ToneParams` of `tone.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ToneParams {
    size: [u32; 2],
    radius: i32,
    window: i32,
    eps: f32,
    omega: f32,
    seed: u32,
    _pad: f32,
}

/// `LlParams` of `ll.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct LlParams {
    fw: u32,
    fh: u32,
    cw: u32,
    ch: u32,
    g_f: u32,
    g_c: u32,
    o_f: u32,
    o_c: u32,
    q_f: u32,
    q_c: u32,
    base: u32,
    _pad: u32,
    sigma: f32,
    clarity: f32,
    _pad2: [f32; 2],
}

/// Longer side of the whole-frame develop the dehaze airlight comes from.
const AIR_SIZE: u32 = 256;

/// Pyramid levels of local contrast at full resolution (features up to 64 px).
const LL_LEVELS: f32 = 6.0;

/// `DetailParams` of `detail.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DetailParams {
    size: [u32; 2],
    sigma: f32,
    radius: i32,
    a: f32,
    b: f32,
    usm: f32,
    usm_threshold: f32,
}

/// `FringeParams` of `fringe.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FringeParams {
    size: [u32; 2],
    radius: i32,
    sigma: f32,
    safety: f32,
    _pad: [f32; 3],
}

/// Rows of a row-major 3×3 matrix, padded as WGSL lays out `mat3x3` columns
/// (the shader multiplies `v * m`, i.e. by the rows).
pub(crate) fn rows(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
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
pub(crate) fn grid(n: u32) -> (u32, u32) {
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

    /// Runs `passes` (pipeline, bindings, grid) in order: in the open batch, or one submission.
    pub(crate) fn passes(&self, passes: &[Pass]) {
        let binds: Vec<_> = passes.iter().map(|(p, b, _)| self.bind(p, b)).collect();
        self.record(|enc| {
            let mut pass = enc.begin_compute_pass(&Default::default());
            for ((p, _, (x, y)), bind) in passes.iter().zip(&binds) {
                pass.set_pipeline(p);
                pass.set_bind_group(0, bind, &[]);
                pass.dispatch_workgroups(*x, *y, 1);
            }
        });
    }

    /// Records into the open batch, or into an encoder submitted at once.
    pub(crate) fn record(&self, f: impl FnOnce(&mut wgpu::CommandEncoder)) {
        let mut batch = self.raw.batch.borrow_mut();
        match batch.as_mut() {
            Some(enc) => f(enc),
            None => {
                let mut enc = self.device.create_command_encoder(&Default::default());
                f(&mut enc);
                self.queue.submit([enc.finish()]);
            }
        }
    }

    /// Reads a buffer of f32 back to the CPU (tests, samples).
    pub fn read_f32(&self, buf: &wgpu::Buffer) -> Vec<f32> {
        bytemuck::cast_slice(&self.read_range(buf, 0, buf.size())).to_vec()
    }

    pub(crate) fn raw_params(&self, raw: &RawData, size: [u32; 4]) -> wgpu::Buffer {
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
                // The uploaded mosaic is spent once reconstructed: it becomes the direction plane.
                let (cfa, dir) = self.reconstruct_keep(raw, mosaic)?;
                let plane = || self.buffer(n as u64 * 4, wgpu::BufferUsages::COPY_SRC);
                let (r, g, b, lpf) = (plane(), plane(), plane(), plane());
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
        let mut gpu_raw = GpuRaw {
            levels: self.mips(raw, level0),
            ai: None,
            camera: raw.camera.clone(),
            orientation: raw.orientation,
            noise: Default::default(),
            id: next_id(),
        };
        // From level 1 (half size: fast, and demosaic artefacts averaged out);
        // its variances are a quarter of full resolution's.
        let k = 1.min(gpu_raw.levels.len() - 1);
        let (_, lw, lh) = gpu_raw.levels[k];
        let m = crate::noise::estimate(&self.read_level(&gpu_raw, k), lw as usize, lh as usize);
        let up = 4f32.powi(k as i32);
        gpu_raw.noise = NoiseModel { a: m.a * up, b: m.b * up };
        Ok(gpu_raw)
    }

    /// The mip chain from `level0` (the visible area of `raw`).
    pub(crate) fn mips(&self, raw: &RawData, level0: wgpu::Buffer) -> Vec<(wgpu::Buffer, u32, u32)> {
        let mut levels = vec![(level0, raw.crop[2], raw.crop[3])];
        while let Some(&(ref src, w, h)) = levels.last().filter(|l| l.1.max(l.2) > MIP_MIN) {
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let dst = self.buffer(nw as u64 * nh as u64 * 8, wgpu::BufferUsages::COPY_SRC);
            let params = self.raw_params(raw, [w, h, nw, nh]);
            self.passes(&[(&self.raw.mip, &[(0, &params), (10, src), (11, &dst)], grid(nw * nh))]);
            levels.push((dst, nw, nh));
        }
        levels
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
        let (sx, sy) = (r.size[0] / w as f32, r.size[1] / h as f32);
        let scale = sx.max(sy);
        // Output pixels per full-resolution pixel; past 100% the tools keep full-resolution radii.
        let s = (1.0 / scale).min(1.0);
        // The AI, once it ran, replaces the classic luminance NR (whose slider it hides).
        let d = &Detail { nr_luma: if raw.ai_active(&dev.detail) { 0.0 } else { dev.detail.nr_luma }, ..dev.detail };
        // The detail tools read around each pixel: develop that far past the edges.
        let margin = (d.support() * s).ceil() as u32;
        let (lw, lh) = (w + 2 * margin, h + 2 * margin);
        self.fits(lw as u64 * lh as u64 * 16)?;
        let (params, k) = develop_params(raw, r, w, h, dev, profile, margin);
        let (level, gains, exposure) = (&raw.levels[k].0, params.gains, params.k);
        self.queue.write_buffer(&self.raw.develop_params, 0, bytemuck::bytes_of(&params));
        // ~11 stages: one submission instead of one each.
        *self.raw.batch.borrow_mut() = Some(self.device.create_command_encoder(&Default::default()));
        let mut oetf = self.raw.oetf.borrow_mut();
        if oetf.as_ref().is_none_or(|(p, _)| *p != profile) {
            *oetf = Some((profile, self.storage(bytemuck::cast_slice(&oetf_table(profile)))));
        }
        let table = &oetf.as_ref().expect("set above").1;
        let photo = self.photo_slot(slot, w, h);
        let mut work = photo.work.borrow_mut();
        if work.as_ref().is_none_or(|x| x.size != (lw, lh)) {
            let usage = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
            let lin = self.buffer(lw as u64 * lh as u64 * 16, usage);
            *work = Some(Work { size: (lw, lh), margin, lin, scratch: None, after_nr: None, after_tone: None });
        }
        let work = work.as_mut().expect("set above");
        work.margin = margin;
        // The sampled level averaged 4^k pixels: a quarter of the variance each;
        // exposure scales the signal, so the model of Y follows it.
        let var = 0.25f32.powi(k as i32);
        let noise = NoiseModel { a: raw.noise.a * var * exposure, b: raw.noise.b * var * exposure * exposure };

        // Slow stages are cached with everything upstream of them.
        let mut key = vec![r.center[0], r.center[1], r.size[0], r.size[1], r.angle, w as f32, h as f32];
        key.extend([k as f32, gains[0], gains[1], gains[2], exposure, d.lens_vignette, margin as f32, params.ai]);
        key.extend([d.nr_luma, d.nr_chroma, d.nr_detail]);
        let nr_key = (raw.id, key.clone());
        key.extend([d.defringe, d.dehaze, d.local_contrast]);
        let tone_key = (raw.id, key);
        let nr_on = d.nr_luma > 0.0 || d.nr_chroma > 0.0;
        let tone_on = d.defringe > 0.0 || d.dehaze != 0.0 || d.local_contrast > 0.0;
        // A tool turned off frees its cache (a 100% view's is ~100 MB).
        if !nr_on {
            work.after_nr = None;
        }
        if !tone_on {
            work.after_tone = None;
        }
        let (p, g) = (&self.raw, grid(lw * lh));
        let hit = |c: &Option<(Key, wgpu::Buffer)>, key: &Key| c.as_ref().filter(|c| c.0 == *key).map(|c| c.1.clone());
        if let Some(b) = hit(&work.after_tone, &tone_key) {
            self.copy(&b, &work.lin);
        } else {
            if let Some(b) = hit(&work.after_nr, &nr_key) {
                self.copy(&b, &work.lin);
            } else {
                let linear = [(10, level), (11, raw.ai_level(k)), (12, &p.develop_params), (15, &work.lin)];
                self.passes(&[(&p.develop_linear, &linear, g)]);
                if nr_on {
                    let lin = work.lin.clone();
                    self.denoise(&lin, work.scratch(self), lw, lh, s, d, noise);
                    keep(self, &work.lin, &mut work.after_nr, nr_key);
                }
            }
            if tone_on {
                let lin = work.lin.clone();
                if d.defringe > 0.0 {
                    self.defringe(&lin, work.scratch(self), lw, lh, s, d.defringe);
                }
                if d.dehaze != 0.0 {
                    self.airlight(raw, dev, profile);
                }
                if d.dehaze != 0.0 || d.local_contrast > 0.0 {
                    self.tone(&lin, work.scratch(self), lw, lh, s, d);
                }
                keep(self, &work.lin, &mut work.after_tone, tone_key);
            }
        }
        if d.sharpen > 0.0 {
            let lin = work.lin.clone();
            self.sharpen(&lin, work.scratch(self), lw, lh, s, d, noise);
        }
        if d.usm > 0.0 {
            let lin = work.lin.clone();
            self.output_sharpen(&lin, work.scratch(self), lw, lh, s, d);
        }
        let encode = [(12, &p.develop_params), (13, &photo.src), (14, table), (15, &work.lin)];
        self.passes(&[(&p.develop_encode, &encode, grid(w * h))]);
        let enc = self.raw.batch.take().expect("opened above");
        self.queue.submit([enc.finish()]);
        Ok(())
    }

    fn copy(&self, from: &wgpu::Buffer, to: &wgpu::Buffer) {
        self.record(|enc| enc.copy_buffer_to_buffer(from, 0, to, 0, from.size()));
    }

    /// The linear result of a develop, without its margin (tests).
    #[cfg(test)]
    pub(crate) fn read_lin(&self, photo: &GpuPhoto) -> Vec<[f32; 4]> {
        let work = photo.work.borrow();
        let work = work.as_ref().expect("developed");
        let all: Vec<[f32; 4]> = bytemuck::cast_slice(&self.read_f32(&work.lin)).to_vec();
        let (m, lw) = (work.margin as usize, work.size.0 as usize);
        let (w, h) = (photo.width as usize, photo.height as usize);
        (0..h).flat_map(|y| all[(y + m) * lw + m..][..w].to_vec()).collect()
    }

    /// Dehaze (`tone.wgsl`): a guided filter of 30 full-resolution pixels,
    /// the dark channel over 15; then local contrast.
    #[allow(clippy::too_many_arguments)]
    fn tone(&self, lin: &wgpu::Buffer, [sa, sb]: [&wgpu::Buffer; 2], w: u32, h: u32, s: f32, detail: &Detail) {
        let (p, g) = (&self.raw, grid(w * h));
        if detail.dehaze != 0.0 {
            let q = self.tone_params(w, h, s, detail.dehaze, 0);
            let (boxes, ab, air) =
                ([(0, &q), (2, sa), (3, sb)], [(0, &q), (2, sa)], [(0, &q), (1, lin), (2, sa), (4, &p.air)]);
            self.passes(&[
                (&p.dh_min_h, &[(0, &q), (1, lin), (3, sb)], g),
                (&p.dh_min_v, &[(0, &q), (2, sa), (3, sb), (4, &p.air)], g),
                (&p.dh_prep, &air, g),
                (&p.box_h, &boxes, g),
                (&p.box_v, &boxes, g),
                (&p.gf_ab, &ab, g),
                (&p.box_h, &boxes, g),
                (&p.box_v, &boxes, g),
                (&p.dh_apply, &air, g),
            ]);
        }
        if detail.local_contrast > 0.0 {
            self.local_laplacian(lin, [sa, sb], w, h, s, detail.local_contrast);
        }
    }

    /// `ToneParams` for a `w` × `h` buffer at `s` output pixels per full-resolution pixel (≤ 1).
    fn tone_params(&self, w: u32, h: u32, s: f32, dehaze: f32, seed: u32) -> wgpu::Buffer {
        self.storage(bytemuck::bytes_of(&ToneParams {
            size: [w, h],
            radius: (30.0 * s).round().max(2.0) as i32,
            window: (7.0 * s).round().max(1.0) as i32,
            eps: 1e-3,
            omega: 0.95 * dehaze / 100.0,
            seed,
            _pad: 0.0,
        }))
    }

    /// Sets the dehaze airlight from the whole frame developed at `AIR_SIZE`
    /// (kept while photo, white balance, exposure, lens and AI blend stay).
    fn airlight(&self, raw: &GpuRaw, dev: &Develop, profile: Profile) {
        let (fw, fh) = raw.oriented_size();
        let whole = Region { center: [fw as f32 / 2.0, fh as f32 / 2.0], size: [fw as f32, fh as f32], angle: 0.0 };
        let (w, h) = crate::photo::Decoded::fit_size(&whole, AIR_SIZE);
        let (params, k) = develop_params(raw, &whole, w, h, dev, profile, 0);
        let g = params.gains;
        let key = (raw.id, [g[0], g[1], g[2], dev.exposure, dev.detail.lens_vignette, params.ai]);
        if self.raw.air_key.replace(Some(key)) == Some(key) {
            return;
        }
        let (params, lin) = (
            self.storage(bytemuck::bytes_of(&params)),
            self.buffer(w as u64 * h as u64 * 16, wgpu::BufferUsages::empty()),
        );
        let (sa, sb) = (
            self.buffer(w as u64 * h as u64 * 16, wgpu::BufferUsages::empty()),
            self.buffer(w as u64 * h as u64 * 16, wgpu::BufferUsages::empty()),
        );
        let q = self.tone_params(w, h, w as f32 / fw as f32, dev.detail.dehaze, 1);
        let (p, g) = (&self.raw, grid(w * h));
        self.queue.write_buffer(&p.air, 0, &[0; 16]);
        self.passes(&[
            (&p.develop_linear, &[(10, &raw.levels[k].0), (11, raw.ai_level(k)), (12, &params), (15, &lin)], g),
            (&p.dh_min_h, &[(0, &q), (1, &lin), (3, &sb)], g),
            (&p.dh_min_v, &[(0, &q), (2, &sa), (3, &sb), (4, &p.air)], g),
        ]);
    }

    /// Local contrast (`ll.wgsl`): darktable's local Laplacian, `LL_LEVELS`
    /// at full resolution, fewer when downscaled. The pyramids live in the
    /// scratch buffers: input and output pyramids (f32) in `sa`, the
    /// remapped ones from level 1 (8 f32 a pixel) in `sb`.
    fn local_laplacian(
        &self,
        lin: &wgpu::Buffer,
        [sa, sb]: [&wgpu::Buffer; 2],
        w: u32,
        h: u32,
        scale: f32,
        amount: f32,
    ) {
        let depth = (LL_LEVELS + scale.log2()).round().max(1.0) as usize;
        let mut dims = vec![(w, h)];
        while let Some(&(a, b)) = dims.last().filter(|&&(a, b)| dims.len() <= depth && a.min(b) >= 4) {
            dims.push((a.div_ceil(2), b.div_ceil(2)));
        }
        let top = dims.len() - 1;
        let n: Vec<u32> = dims.iter().map(|&(a, b)| a * b).collect();
        let total: u32 = n.iter().sum();
        let offsets = |from: usize| -> Vec<u32> { (0..dims.len()).map(|l| n[from.min(l)..l].iter().sum()).collect() };
        let (g_off, q_off) = (offsets(0), offsets(1));
        let params = |l: usize, c: usize| {
            self.storage(bytemuck::bytes_of(&LlParams {
                fw: dims[l].0,
                fh: dims[l].1,
                cw: dims[c].0,
                ch: dims[c].1,
                g_f: g_off[l],
                g_c: g_off[c],
                o_f: total + g_off[l],
                o_c: total + g_off[c],
                q_f: q_off[l],
                q_c: q_off[c],
                base: (l == 0) as u32,
                sigma: 0.2,
                clarity: amount / 100.0,
                ..Default::default()
            }))
        };
        let down: Vec<_> = (0..top).map(|l| params(l, l + 1)).collect();
        let (first, last) = (params(0, 0), params(top, top));
        let p = &self.raw;
        let mut passes: Vec<Pass> = Vec::new();
        let prep = [(0, &first), (1, lin), (2, sa)];
        passes.push((&p.ll_prep, &prep, grid(n[0])));
        let g_binds: Vec<[(u32, &wgpu::Buffer); 2]> = down.iter().map(|q| [(0, q), (2, sa)]).collect();
        let q_binds: Vec<[(u32, &wgpu::Buffer); 3]> = down.iter().map(|q| [(0, q), (2, sa), (3, sb)]).collect();
        for l in 0..top {
            passes.push((&p.ll_reduce_g, &g_binds[l], grid(n[l + 1])));
            passes.push((&p.ll_reduce_q, &q_binds[l], grid(n[l + 1])));
        }
        let top_bind = [(0, &last), (2, sa)];
        passes.push((&p.ll_top, &top_bind, grid(n[top])));
        for l in (0..top).rev() {
            passes.push((&p.ll_collapse, &q_binds[l], grid(n[l])));
        }
        passes.push((&p.ll_apply, &prep, grid(n[0])));
        self.passes(&passes);
    }

    /// `DetailParams` of `detail.wgsl`.
    fn detail_params(w: u32, h: u32, sigma: f32, noise: NoiseModel) -> DetailParams {
        DetailParams {
            size: [w, h],
            sigma,
            radius: (3.0 * sigma).ceil() as i32,
            a: noise.a,
            b: noise.b,
            usm: 0.0,
            usm_threshold: 0.0,
        }
    }

    /// Defringe (`fringe.wgsl`): darktable's chromatic aberrations module,
    /// radius 5 px at full resolution, strength = `amount` / 100 (50 is darktable's default).
    fn defringe(&self, lin: &wgpu::Buffer, [sa, sb]: [&wgpu::Buffer; 2], w: u32, h: u32, s: f32, amount: f32) {
        let sigma = (5.0 * s).max(1.0);
        let pr = self.storage(bytemuck::bytes_of(&FringeParams {
            size: [w, h],
            radius: (3.0 * sigma).ceil() as i32,
            sigma,
            safety: 20f32.powf(1.0 - amount / 100.0),
            _pad: [0.0; 3],
        }));
        // Blurred input, higher and lower manifolds.
        let [b, hi, lo] = [0; 3].map(|_| self.buffer(lin.size(), wgpu::BufferUsages::empty()));
        let (p, g) = (&self.raw, grid(w * h));
        let blur_h = |from, to| (&p.fr_blur_h, vec![(0, &pr), (4, from), (5, to)]);
        let blur_v = |from, to| (&p.fr_blur_v, vec![(0, &pr), (4, from), (5, to)]);
        let passes = [
            blur_h(lin, sb),
            blur_v(sb, &b),
            (&p.fr_manifolds, vec![(0, &pr), (1, lin), (2, &lo), (5, &hi), (6, &b)]),
            blur_h(&hi, sb),
            blur_v(sb, &hi),
            blur_h(&lo, sb),
            blur_v(sb, &lo),
            (&p.fr_normalise, vec![(0, &pr), (2, &lo), (5, &hi), (6, &b)]),
            (&p.fr_out, vec![(0, &pr), (1, lin), (2, sa), (4, &hi), (6, &lo)]),
            blur_h(sa, sb),
            blur_v(sb, sa),
            (&p.fr_apply, vec![(0, &pr), (1, lin), (2, sa), (4, &hi), (6, &lo)]),
        ];
        let passes: Vec<Pass> = passes.iter().map(|(pl, b)| (*pl, b.as_slice(), g)).collect();
        self.passes(&passes);
    }

    /// Capture sharpening (`detail.wgsl`): Richardson–Lucy, one iteration per 10 of the amount.
    #[allow(clippy::too_many_arguments)]
    fn sharpen(
        &self,
        lin: &wgpu::Buffer,
        [sa, sb]: [&wgpu::Buffer; 2],
        w: u32,
        h: u32,
        s: f32,
        detail: &Detail,
        noise: NoiseModel,
    ) {
        let sigma = (detail.sharpen_radius * s).max(0.3);
        // The deconvolution converges in a few iterations: the amount blends it in instead.
        let params = DetailParams { usm: detail.sharpen / 100.0, ..Self::detail_params(w, h, sigma, noise) };
        let params = self.storage(bytemuck::bytes_of(&params));
        let (p, g) = (&self.raw, grid(w * h));
        let (own, both) = ([(0, &params), (1, lin), (2, sa)], [(0, &params), (2, sa), (3, sb)]);
        let mut passes: Vec<Pass> = vec![(&p.sh_init, &own, g)];
        for _ in 0..10 {
            passes.extend([(&p.sh_ratio, both.as_slice(), g), (&p.sh_update, both.as_slice(), g)]);
        }
        passes.push((&p.sh_apply, &own, g));
        self.passes(&passes);
    }

    /// Output sharpening (`detail.wgsl`): unsharp mask of L = Y^(1/3), up to 2× the detail at 100.
    #[allow(clippy::too_many_arguments)]
    fn output_sharpen(
        &self,
        lin: &wgpu::Buffer,
        [sa, sb]: [&wgpu::Buffer; 2],
        w: u32,
        h: u32,
        s: f32,
        detail: &Detail,
    ) {
        let sigma = (detail.usm_radius * s).max(0.3);
        let params = self.storage(bytemuck::bytes_of(&DetailParams {
            usm: 2.0 * detail.usm / 100.0,
            // 0..100 → up to 0.02 of L (about 5% of a mid-grey's lightness).
            usm_threshold: detail.usm_threshold / 100.0 * 0.02,
            ..Self::detail_params(w, h, sigma, NoiseModel::default())
        }));
        let (p, g) = (&self.raw, grid(w * h));
        let (own, both) = ([(0, &params), (1, lin), (2, sa)], [(0, &params), (2, sa), (3, sb)]);
        self.passes(&[
            (&p.us_init, &own, g),
            (&p.us_blur_h, &both, g),
            (&p.us_blur_v, &both, g),
            (&p.us_apply, &own, g),
        ]);
    }

    /// Classic noise reduction (`denoise.wgsl`); radii shrink with `scale`.
    #[allow(clippy::too_many_arguments)]
    fn denoise(
        &self,
        lin: &wgpu::Buffer,
        [sa, sb]: [&wgpu::Buffer; 2],
        w: u32,
        h: u32,
        scale: f32,
        detail: &Detail,
        noise: NoiseModel,
    ) {
        #[cfg(test)]
        self.raw.nr_runs.set(self.raw.nr_runs.get() + 1);
        let radius = |full: f32| (full * scale).round().max(1.0) as i32;
        let params = |step: i32| {
            self.storage(bytemuck::bytes_of(&NrParams {
                size: [w, h],
                patch: radius(2.0),
                search: radius(7.0),
                step,
                _pad: 0,
                a: noise.a,
                b: noise.b,
                // darktable's denoiseprofile strength: NLM as if the noise were ×¼ (amount 0)
                // to ×4 (100) of the measured one, ×1 at 50.
                luma: 4f32.powf(2.0 * detail.nr_luma / 100.0 - 1.0),
                detail: detail.nr_detail / 200.0,
                chroma: detail.nr_chroma / 100.0,
                _pad2: 0.0,
            }))
        };
        let (p, g, base) = (&self.raw, grid(w * h), params(1));
        if detail.nr_luma > 0.0 {
            // Each pass binds only what it uses (layouts are derived from the shader).
            self.passes(&[
                (&p.nr_gat, &[(0, &base), (1, lin), (2, sa)], g),
                (&p.nr_nlm, &[(0, &base), (2, sa), (3, sb)], (w.div_ceil(16), h.div_ceil(16))),
                (&p.nr_luma_apply, &[(0, &base), (1, lin), (3, sb)], g),
            ]);
        }
        if detail.nr_chroma > 0.0 {
            // Five levels at full resolution (31 px), fewer when downscaled.
            let levels = (5.0 + scale.log2()).round().clamp(1.0, 5.0) as i32;
            let steps: Vec<_> = (0..levels).map(|l| params(1 << l)).collect();
            let mut passes: Vec<Pass> = Vec::new();
            let init = [(0, &base), (1, lin), (2, sa)];
            passes.push((&p.nr_chroma_init, &init, g));
            let order = [[(0, sa), (1, sb)], [(0, sb), (1, sa)]];
            let binds: Vec<[(u32, &wgpu::Buffer); 3]> = steps
                .iter()
                .enumerate()
                .map(|(l, s)| {
                    let [(_, from), (_, to)] = order[l % 2];
                    [(0, s), (2, from), (3, to)]
                })
                .collect();
            passes.extend(binds.iter().map(|b| (&p.nr_atrous, b.as_slice(), g)));
            // The last level wrote the buffer the next one would read: bind it as `sa`.
            let last = if levels % 2 == 1 { sb } else { sa };
            let apply = [(0, &base), (1, lin), (2, last)];
            passes.push((&p.nr_chroma_apply, &apply, g));
            self.passes(&passes);
        }
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
        self.reconstruct_keep(raw, mosaic).map(|(out, _)| out)
    }

    /// [`Gpu::reconstruct`], also handing back the uploaded mosaic's buffer
    /// (free once it ran) for the next passes to reuse.
    pub(crate) fn reconstruct_keep(
        &self,
        raw: &RawData,
        mosaic: &[f32],
    ) -> Result<(wgpu::Buffer, wgpu::Buffer), String> {
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
        Ok((out, cfa))
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
pub(crate) fn unpack(w: &[u32]) -> [f32; 3] {
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
            for dev in [Develop::default(), Develop { wb: Some([3200.0, 10.0]), exposure: 0.7, ..Default::default() }] {
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

    /// Detail tools at zero run nothing: the photo is bit-exact whatever
    /// their other settings (a radius alone changes nothing).
    #[test]
    fn neutral_detail_is_identity() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (16, 8);
        let px = (0..w * h).map(|i| [(i % w) as f32 / 16.0, (i / w) as f32 / 8.0, 0.3]).collect();
        let raw = rgb_raw(&gpu, w, h, px, 1, Camera::neutral());
        let plain = Develop::default();
        let idle = Develop { detail: Detail { sharpen_radius: 2.0, ..Detail::default() }, ..plain };
        for profile in [Profile::Standard, Profile::VLog] {
            let run = |dev: &Develop| gpu.read_photo(&gpu.develop(&raw, &whole(w, h), w, h, dev, profile).unwrap());
            assert_eq!(run(&plain).pixels, run(&idle).pixels, "{profile:?}");
        }
    }

    /// A RAW whose AI chain is another photo's: what the AI blend picks shows.
    fn with_ai_chain(gpu: &Gpu, w: u32, h: u32) -> GpuRaw {
        let px = |v: f32| (0..w * h).map(|i| [(i % w) as f32 / w as f32, v, 0.3]).collect();
        let mut raw = rgb_raw(gpu, w, h, px(0.2), 1, Camera::neutral());
        raw.ai = Some(rgb_raw(gpu, w, h, px(0.6), 1, Camera::neutral()).levels);
        raw
    }

    fn ai(on: bool, strength: f32) -> Develop {
        Develop {
            detail: Detail { nr_ai: on, nr_ai_strength: strength, nr_luma: 40.0, ..Detail::default() },
            ..Develop::default()
        }
    }

    /// AI off develops as if it never ran; classic luminance NR applies.
    #[test]
    fn ai_off_is_bit_exact() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (64, 32);
        let mut raw = with_ai_chain(&gpu, w, h);
        let run = |raw: &GpuRaw, dev: &Develop| {
            gpu.read_photo(&gpu.develop(raw, &whole(w, h), w, h, dev, Profile::Standard).unwrap()).pixels
        };
        let with = run(&raw, &ai(false, 100.0));
        raw.ai = None;
        assert_eq!(with, run(&raw, &ai(false, 100.0)));
    }

    /// Strength 0 is the classic demosaic (without luminance NR); 100 the AI chain.
    #[test]
    fn ai_strength_zero_equals_rcd() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (64, 32);
        let raw = with_ai_chain(&gpu, w, h);
        let run = |dev: &Develop| {
            gpu.read_photo(&gpu.develop(&raw, &whole(w, h), w, h, dev, Profile::Standard).unwrap()).pixels
        };
        let classic = Develop::default();
        assert_eq!(run(&ai(true, 0.0)), run(&classic));
        assert_ne!(run(&ai(true, 100.0)), run(&classic));
    }

    /// The dehaze airlight follows the AI blend: AI on dehazes the same
    /// whether or not the classic develop ran first.
    #[test]
    fn dehaze_airlight_follows_ai() {
        let (Ok(a), Ok(b)) = (Gpu::new(), Gpu::new()) else { return eprintln!("no GPU, skipped") };
        let (w, h) = (64, 32);
        let hazy = |on| Develop { detail: Detail { dehaze: 60.0, ..ai(on, 100.0).detail }, ..Develop::default() };
        let run = |gpu: &Gpu, raw: &GpuRaw, dev: &Develop| {
            gpu.read_photo(&gpu.develop(raw, &whole(w, h), w, h, dev, Profile::Standard).unwrap()).pixels
        };
        let raw_a = with_ai_chain(&a, w, h);
        run(&a, &raw_a, &hazy(false));
        let after_classic = run(&a, &raw_a, &hazy(true));
        assert_eq!(after_classic, run(&b, &with_ai_chain(&b, w, h), &hazy(true)));
    }

    fn flat_lens(gpu: &Gpu, amount: f32) -> Vec<[f32; 4]> {
        let (w, h) = (30, 20);
        let raw = rgb_raw(gpu, w, h, vec![[0.2; 3]; (w * h) as usize], 1, Camera::neutral());
        let dev = Develop { detail: Detail { lens_vignette: amount, ..Detail::default() }, ..Develop::default() };
        gpu.read_photo(&gpu.develop(&raw, &whole(w, h), w, h, &dev, Profile::VLog).unwrap()).pixels
    }

    #[test]
    fn lens_vignette_zero_is_identity() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let base = flat_lens(&gpu, 0.0);
        assert!(base.iter().all(|p| *p == base[0]), "a flat field stays flat");
    }

    /// Positive amounts brighten towards the corners, the same at the same
    /// distance from the centre; negative ones darken.
    #[test]
    fn lens_vignette_is_radially_symmetric() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (30usize, 20usize);
        let at = |px: &[[f32; 4]], x: usize, y: usize| px[y * w + x][1];
        let up = flat_lens(&gpu, 50.0);
        let (centre, corner) = (at(&up, w / 2, h / 2), at(&up, 0, 0));
        assert!(corner > centre + 0.01, "corner {corner} vs centre {centre}");
        for (x, y) in [(0, 0), (3, 5), (10, 2)] {
            let mirrored = [at(&up, w - 1 - x, y), at(&up, x, h - 1 - y), at(&up, w - 1 - x, h - 1 - y)];
            assert!(mirrored.iter().all(|v| (v - at(&up, x, y)).abs() < 1e-5), "({x}, {y}): {mirrored:?}");
        }
        assert!(at(&flat_lens(&gpu, -50.0), 0, 0) < centre - 0.01);
        // V-Log encodes linear 0 as 0.125: the strongest darkening keeps the corners above black.
        assert!(at(&flat_lens(&gpu, -100.0), 0, 0) > 0.13);
    }

    const NOISE: crate::noise::NoiseModel = crate::noise::NoiseModel { a: 0.002, b: 0.0002 };

    /// A `w` × `h` field of `f(x)` with Poisson–Gaussian noise of [`NOISE`], developed
    /// at full resolution with `detail`; the linear Rec.2020 result.
    fn noisy(gpu: &Gpu, w: u32, h: u32, f: impl Fn(u32) -> f32, detail: Detail) -> Vec<[f32; 4]> {
        let mut n = crate::noise::tests::normals(11);
        let px = (0..w * h)
            .map(|i| {
                let x = f(i % w);
                std::array::from_fn(|_| x + (NOISE.a * x + NOISE.b).sqrt() * n())
            })
            .collect();
        let mut raw = rgb_raw(gpu, w, h, px, 1, Camera::neutral());
        raw.noise = NOISE;
        let dev = Develop { detail, ..Develop::default() };
        let photo = gpu.develop(&raw, &whole(w, h), w, h, &dev, Profile::Standard).unwrap();
        gpu.read_lin(&photo)
    }

    fn luma(p: &[f32; 4]) -> f32 {
        0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2]
    }

    fn mean_std(v: impl Iterator<Item = f32> + Clone) -> (f32, f32) {
        let n = v.clone().count() as f32;
        let m = v.clone().sum::<f32>() / n;
        (m, (v.map(|x| (x - m).powi(2)).sum::<f32>() / n).sqrt())
    }

    fn nr(luma: f32, chroma: f32) -> Detail {
        Detail { nr_luma: luma, nr_chroma: chroma, ..Detail::default() }
    }

    #[test]
    fn nr_preserves_the_mean() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let before = mean_std(noisy(&gpu, 96, 96, |_| 0.2, nr(0.0, 0.0)).iter().map(luma)).0;
        let after = mean_std(noisy(&gpu, 96, 96, |_| 0.2, nr(100.0, 100.0)).iter().map(luma)).0;
        assert!((after / before - 1.0).abs() < 0.005, "{after} vs {before}");
    }

    #[test]
    fn nr_reduces_noise() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let before = mean_std(noisy(&gpu, 96, 96, |_| 0.2, nr(0.0, 0.0)).iter().map(luma)).1;
        let after = mean_std(noisy(&gpu, 96, 96, |_| 0.2, nr(100.0, 0.0)).iter().map(luma)).1;
        assert!(after * 3.0 <= before, "std {after} vs {before}");
    }

    /// 10–90% rise of a step edge, from the row-averaged profile.
    fn rise(px: &[[f32; 4]], w: usize) -> f32 {
        let h = px.len() / w;
        let profile: Vec<f32> = (0..w).map(|x| (0..h).map(|y| luma(&px[y * w + x])).sum::<f32>() / h as f32).collect();
        let (lo, hi) = (
            profile[..w / 4].iter().sum::<f32>() / (w / 4) as f32,
            profile[3 * w / 4..].iter().sum::<f32>() / (w / 4) as f32,
        );
        let cross = |t: f32| {
            let level = lo + t * (hi - lo);
            let i = profile.iter().position(|&v| v >= level).unwrap();
            i as f32 - 1.0 + (level - profile[i - 1]) / (profile[i] - profile[i - 1])
        };
        cross(0.9) - cross(0.1)
    }

    #[test]
    fn nr_keeps_edges() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let step = |x: u32| if x < 48 { 0.1 } else { 0.4 };
        let before = rise(&noisy(&gpu, 96, 64, step, nr(0.0, 0.0)), 96);
        let after = rise(&noisy(&gpu, 96, 64, step, nr(100.0, 100.0)), 96);
        assert!(after <= before + 1.0, "rise {after} vs {before}");
    }

    #[test]
    fn chroma_nr_leaves_luminance() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let f = |x: u32| 0.1 + x as f32 / 200.0;
        let before = noisy(&gpu, 64, 48, f, nr(0.0, 0.0));
        let after = noisy(&gpu, 64, 48, f, nr(0.0, 100.0));
        assert!(before.iter().zip(&after).all(|(p, q)| (luma(p) - luma(q)).abs() < 1e-3));
        let chroma = |v: &[[f32; 4]]| mean_std(v.iter().map(|p| p[2] - luma(p))).1;
        assert!(chroma(&after) * 2.0 < chroma(&before), "chroma noise {} vs {}", chroma(&after), chroma(&before));
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

    /// A field of exact pixels (no noise), developed with `detail`; linear result.
    fn field(gpu: &Gpu, w: u32, h: u32, px: Vec<[f32; 3]>, detail: Detail) -> Vec<[f32; 4]> {
        let mut raw = rgb_raw(gpu, w, h, px, 1, Camera::neutral());
        raw.noise = crate::noise::NoiseModel { a: 1e-6, b: 1e-8 };
        let dev = Develop { detail, ..Develop::default() };
        let photo = gpu.develop(&raw, &whole(w, h), w, h, &dev, Profile::Standard).unwrap();
        gpu.read_lin(&photo)
    }

    /// A step from 0.1 to 0.5 at x = 32, blurred by a Gaussian of σ 0.8 px
    /// (the default capture-sharpening PSF).
    fn soft_step(w: u32, h: u32) -> Vec<[f32; 3]> {
        let erf = |x: f32| {
            (1.0 - (-x * x * (4.0 / std::f32::consts::PI + 0.147 * x * x) / (1.0 + 0.147 * x * x)).exp()).sqrt()
                * x.signum()
        };
        (0..w * h)
            .map(|i| [0.1 + 0.4 * 0.5 * (1.0 + erf(((i % w) as f32 + 0.5 - 32.0) / (0.8 * 2f32.sqrt()))); 3])
            .collect()
    }

    fn sharp(amount: f32) -> Detail {
        Detail { sharpen: amount, ..Detail::default() }
    }

    fn usm(amount: f32, threshold: f32) -> Detail {
        Detail { usm: amount, usm_threshold: threshold, ..Detail::default() }
    }

    /// Output sharpening steepens a soft edge visibly (10–90% rise ≥ 30% shorter at 100).
    #[test]
    fn output_sharpening_steepens_a_soft_edge() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let before = rise(&field(&gpu, 64, 16, soft_step(64, 16), usm(0.0, 0.0)), 64);
        let after = rise(&field(&gpu, 64, 16, soft_step(64, 16), usm(100.0, 0.0)), 64);
        assert!(after < 0.7 * before, "rise {after} vs {before}");
    }

    /// A flat field stays flat, and its mean stays put on a step.
    #[test]
    fn output_sharpening_keeps_flat_areas_and_the_mean() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let flat = field(&gpu, 32, 16, vec![[0.3; 3]; 32 * 16], usm(100.0, 0.0));
        assert!(flat.iter().all(|p| (p[1] - flat[0][1]).abs() < 1e-5));
        let m = |px: &[[f32; 4]]| px.iter().map(luma).sum::<f32>() / px.len() as f32;
        let (a, b) = (
            m(&field(&gpu, 64, 16, soft_step(64, 16), usm(0.0, 0.0))),
            m(&field(&gpu, 64, 16, soft_step(64, 16), usm(100.0, 0.0))),
        );
        assert!((a - b).abs() < 0.01 * a, "mean {b} vs {a}");
    }

    /// The threshold leaves fine noise alone: with it, a noisy flat field barely changes.
    #[test]
    fn output_sharpening_threshold_spares_noise() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let mut n = crate::noise::tests::normals(3);
        let px: Vec<[f32; 3]> = (0..64 * 64).map(|_| [0.3 + 0.003 * n(); 3]).collect();
        let std = |d| mean_std(field(&gpu, 64, 64, px.clone(), d).iter().map(luma)).1;
        let (base, raw_usm, held) = (std(usm(0.0, 0.0)), std(usm(100.0, 0.0)), std(usm(100.0, 50.0)));
        assert!(raw_usm > 1.3 * base, "unthresholded {raw_usm} vs {base}");
        assert!(held < 1.05 * base, "thresholded {held} vs {base}");
    }

    #[test]
    fn sharpening_raises_edge_acutance() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let before = rise(&field(&gpu, 64, 16, soft_step(64, 16), sharp(0.0)), 64);
        let after = rise(&field(&gpu, 64, 16, soft_step(64, 16), sharp(100.0)), 64);
        assert!(after <= 0.75 * before, "rise {after} vs {before}");
    }

    #[test]
    fn sharpening_overshoot_is_bounded() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let range = |d: Detail| {
            let out = field(&gpu, 64, 16, soft_step(64, 16), d);
            out.iter().map(luma).fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(v), h.max(v)))
        };
        let ((lo, hi), (a, b)) = (range(sharp(0.0)), range(sharp(100.0)));
        let slack = 0.05 * (hi - lo);
        assert!(a >= lo - slack && b <= hi + slack, "{a}..{b} for a {lo}..{hi} step");
    }

    #[test]
    fn sharpening_leaves_flat_noise() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let before = mean_std(noisy(&gpu, 96, 96, |_| 0.2, sharp(0.0)).iter().map(luma)).1;
        let after = mean_std(noisy(&gpu, 96, 96, |_| 0.2, sharp(100.0)).iter().map(luma)).1;
        assert!(after <= 1.05 * before, "std {after} vs {before}");
    }

    /// A purple fringe along a dark/bright edge loses its colour; a purple
    /// patch away from any edge (same luminance as its surround) keeps it.
    #[test]
    fn defringe_cleans_purple_edges_only() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (96u32, 48u32);
        let purple = [0.35, 0.08, 0.6];
        let y = 0.2627 * purple[0] + 0.678 * purple[1] + 0.0593 * purple[2];
        let px = (0..w * h)
            .map(|i| match (i % w, i / w) {
                (30..=31, _) => purple,
                (0..=31, _) => [0.03; 3],
                (60..=79, 14..=33) => purple,
                (60.., _) => [y; 3],
                _ => [0.8; 3],
            })
            .collect::<Vec<_>>();
        let chroma = |v: &[[f32; 4]], x: u32, yy: u32| {
            let p = v[(yy * w + x) as usize];
            ((p[0] - p[1]).powi(2) + (p[2] - p[1]).powi(2)).sqrt()
        };
        let before = field(&gpu, w, h, px.clone(), Detail::default());
        let after = field(&gpu, w, h, px, Detail { defringe: 100.0, ..Detail::default() });
        assert!(
            chroma(&after, 31, 24) <= 0.4 * chroma(&before, 31, 24),
            "fringe {} vs {}",
            chroma(&after, 31, 24),
            chroma(&before, 31, 24)
        );
        assert!(
            chroma(&after, 70, 24) >= 0.9 * chroma(&before, 70, 24),
            "patch {} vs {}",
            chroma(&after, 70, 24),
            chroma(&before, 70, 24)
        );
    }

    /// Dark branch, purple fringe, bright blue sky: the fringe loses its
    /// colour without taking the sky's, and the dark side stays neutral.
    #[test]
    fn defringe_does_not_paint_the_bright_side_colour() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (96u32, 32u32);
        let px = (0..w * h)
            .map(|i| match i % w {
                0..=45 => [0.03; 3],
                46..=47 => [0.35, 0.08, 0.6],
                _ => [0.5, 0.7, 0.95],
            })
            .collect::<Vec<_>>();
        let before = field(&gpu, w, h, px.clone(), Detail::default());
        let after = field(&gpu, w, h, px, Detail { defringe: 100.0, ..Detail::default() });
        let chroma = |p: [f32; 4]| (p[0] - p[1]).abs().max((p[2] - p[1]).abs()) / p[1].max(1e-6);
        let at = |v: &[[f32; 4]], x: u32| v[(16 * w + x) as usize];
        assert!(chroma(at(&after, 47)) < 0.5 * chroma(at(&before, 47)), "fringe {:?}", at(&after, 47));
        for x in 30..=44 {
            assert!(chroma(at(&after, x)) < 0.01, "dark side tinted at {x}: {:?}", at(&after, x));
        }
    }

    /// A hazy horizontal gradient: scene 0.05..0.5 seen through haze t = 0.5, airlight 0.8.
    fn hazy(w: u32, h: u32) -> Vec<[f32; 3]> {
        (0..w * h)
            .map(|i| {
                let j = 0.05 + 0.45 * (i % w) as f32 / w as f32 * (1.0 + 0.3 * ((i / w) as f32 * 0.7).sin());
                [j * 0.5 + 0.8 * 0.5; 3]
            })
            .collect()
    }

    fn std_y(px: &[[f32; 4]]) -> f32 {
        mean_std(px.iter().map(luma)).1
    }

    #[test]
    fn dehaze_raises_contrast_of_hazy_gradient() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let before = std_y(&field(&gpu, 64, 32, hazy(64, 32), Detail::default()));
        let after = std_y(&field(&gpu, 64, 32, hazy(64, 32), Detail { dehaze: 100.0, ..Detail::default() }));
        assert!(after > 1.3 * before, "std {after} vs {before}");
    }

    #[test]
    fn negative_dehaze_lowers_contrast() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let before = std_y(&field(&gpu, 64, 32, hazy(64, 32), Detail::default()));
        let after = std_y(&field(&gpu, 64, 32, hazy(64, 32), Detail { dehaze: -100.0, ..Detail::default() }));
        assert!(after < 0.8 * before, "std {after} vs {before}");
    }

    /// Fine texture around 0.2: its mean stays, its contrast grows.
    fn texture(w: u32, h: u32) -> Vec<[f32; 3]> {
        (0..w * h)
            .map(|i| [0.2 * (1.0 + 0.15 * ((i % w) as f32 * 1.3).sin() * ((i / w) as f32 * 0.9).cos()); 3])
            .collect()
    }

    #[test]
    fn local_contrast_keeps_the_mean() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let lc = Detail { local_contrast: 100.0, ..Detail::default() };
        let (before, after) =
            (field(&gpu, 64, 64, texture(64, 64), Detail::default()), field(&gpu, 64, 64, texture(64, 64), lc));
        let (m0, m1) = (mean_std(before.iter().map(luma)).0, mean_std(after.iter().map(luma)).0);
        assert!((m1 / m0 - 1.0).abs() < 0.01, "mean {m1} vs {m0}");
        assert!(std_y(&after) > 1.3 * std_y(&before), "texture {} vs {}", std_y(&after), std_y(&before));
    }

    /// The local Laplacian lifts the bright side of a 12× step a little (it
    /// is brighter than its 64 px surround, as in darktable), but never
    /// inverts the edge: at 100 (darktable's detail 200%) within 10%.
    #[test]
    fn local_contrast_has_no_halo() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let step = |x: u32| if x < 32 { 0.05 } else { 0.6 };
        let px: Vec<[f32; 3]> = (0..64 * 16).map(|i| [step(i % 64); 3]).collect();
        let row = |d: Detail| field(&gpu, 64, 16, px.clone(), d)[8 * 64..9 * 64].iter().map(luma).collect::<Vec<_>>();
        let (before, after) = (row(Detail::default()), row(Detail { local_contrast: 100.0, ..Detail::default() }));
        let (lo, hi) = (before[0], before[63]);
        let slack = 0.1 * (hi - lo);
        assert!(after.iter().all(|&v| v >= lo - slack && v <= hi + slack), "{after:?} for a {lo}..{hi} step");
        assert!(after[..32].iter().all(|&d| after[32..].iter().all(|&b| b > d)), "edge inverted: {after:?}");
    }

    fn all_tools() -> Detail {
        Detail {
            nr_luma: 60.0,
            nr_chroma: 60.0,
            sharpen: 60.0,
            defringe: 50.0,
            local_contrast: 50.0,
            ..Detail::default()
        }
    }

    /// Changing only sharpening re-runs sharpening, not noise reduction.
    #[test]
    fn cached_stage_is_reused() {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (64, 48);
        let raw = rgb_raw(&gpu, w, h, texture(w, h), 1, Camera::neutral());
        let mut slot = None;
        let mut dev = Develop { detail: all_tools(), ..Develop::default() };
        gpu.develop_into(&mut slot, &raw, &whole(w, h), w, h, &dev, Profile::Standard).unwrap();
        let runs = gpu.raw.nr_runs.get();
        dev.detail.sharpen = 30.0;
        gpu.develop_into(&mut slot, &raw, &whole(w, h), w, h, &dev, Profile::Standard).unwrap();
        assert_eq!(gpu.raw.nr_runs.get(), runs, "noise reduction ran again");
        // And the cached result is the same as a fresh one.
        let fresh = gpu.read_photo(&gpu.develop(&raw, &whole(w, h), w, h, &dev, Profile::Standard).unwrap());
        assert_eq!(gpu.read_photo(slot.as_ref().unwrap()).pixels, fresh.pixels);
        dev.exposure = 0.5;
        gpu.develop_into(&mut slot, &raw, &whole(w, h), w, h, &dev, Profile::Standard).unwrap();
        assert_eq!(gpu.raw.nr_runs.get(), runs + 2, "an exposure change re-runs everything");
    }

    /// A 1:1 view of part of the photo is that part of the full develop: the
    /// detail tools see past the view's edges (no seams when zoomed).
    #[test]
    fn view_matches_fit_crop() {
        view_vs_fit(all_tools(), 50);
    }

    /// The dehaze airlight is the whole frame's: a view without the bright
    /// side dehazes like the fit view.
    #[test]
    fn dehaze_view_matches_fit() {
        view_vs_fit(Detail { dehaze: 60.0, ..Detail::default() }, 5);
    }

    /// A 1:1 view at `x0` (the bright side starts at 70) equals the fit develop cropped.
    fn view_vs_fit(detail: Detail, x0: u32) {
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let (w, h) = (160, 120);
        let mut n = crate::noise::tests::normals(5);
        let px: Vec<[f32; 3]> = texture(w, h)
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let edge = if (i as u32 % w) < 70 { 0.6 } else { 1.0 };
                p.map(|v| v * edge + 0.01 * n())
            })
            .collect();
        let mut raw = rgb_raw(&gpu, w, h, px, 1, Camera::neutral());
        raw.noise = NOISE;
        let dev = Develop { detail, ..Develop::default() };
        let full = gpu.read_photo(&gpu.develop(&raw, &whole(w, h), w, h, &dev, Profile::Standard).unwrap());
        let (y0, vw, vh) = (30u32, 48u32, 40u32);
        let view = Region {
            center: [x0 as f32 + vw as f32 / 2.0, y0 as f32 + vh as f32 / 2.0],
            size: [vw as f32, vh as f32],
            angle: 0.0,
        };
        let part = gpu.read_photo(&gpu.develop(&raw, &view, vw, vh, &dev, Profile::Standard).unwrap());
        for y in 0..vh {
            for x in 0..vw {
                let (a, b) = (part.pixels[(y * vw + x) as usize], full.pixels[((y + y0) * w + x + x0) as usize]);
                assert!((0..3).all(|c| (a[c] - b[c]).abs() < 2e-3), "({x}, {y}): {a:?} vs {b:?}");
            }
        }
    }

    /// On a real file (RAWVALLEY_RAW): the estimated noise model, and what
    /// noise reduction costs a 2048 px fit and a 100% 2048 px view.
    #[test]
    fn nr_on_sample() {
        let Ok(path) = std::env::var("RAWVALLEY_RAW") else { return eprintln!("no sample, skipped") };
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let t = std::time::Instant::now();
        let raw = gpu.load_raw(&read_raw(Path::new(&path)).unwrap()).unwrap();
        eprintln!("load {:?}, noise {:?}", t.elapsed(), raw.noise);
        assert!(raw.noise.a > 0.0 || raw.noise.b > 0.0);
        let (w, h) = raw.oriented_size();
        let fit = whole(w, h);
        let (fw, fh) = crate::photo::Decoded::fit_size(&fit, 2048);
        let view = Region { center: fit.center, size: [2048.0, 1365.0], angle: 0.0 };
        let dev = Develop { detail: nr(50.0, 50.0), ..Develop::default() };
        for (name, r, ow, oh) in [("fit", fit, fw, fh), ("100%", view, 2048, 1365)] {
            let mut slot = None;
            gpu.develop_into(&mut slot, &raw, &r, ow, oh, &dev, Profile::Standard).unwrap();
            let t = std::time::Instant::now();
            for _ in 0..3 {
                gpu.develop_into(&mut slot, &raw, &r, ow, oh, &dev, Profile::Standard).unwrap();
                gpu.read_range(&slot.as_ref().unwrap().src, 0, 16);
            }
            eprintln!("NR {name} {ow}×{oh}: {:?}", t.elapsed() / 3);
        }
    }

    /// On a real file (RAWVALLEY_RAW), NR 50: a ¼-scale view and a 100% view
    /// of the same region, box-averaged to ¼, keep the same noise on flat
    /// patches (within 25%) — the fit predicts the zoom. Also prints the
    /// Balanced drag-frame time at 2048 px with all tools on.
    #[test]
    fn fit_and_zoom_agree_statistically() {
        let Ok(path) = std::env::var("RAWVALLEY_RAW") else { return eprintln!("no sample, skipped") };
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let raw = gpu.load_raw(&read_raw(Path::new(&path)).unwrap()).unwrap();
        let (w, h) = raw.oriented_size();
        let r = Region { center: [w as f32 / 2.0, h as f32 / 2.0], size: [2048.0, 1364.0], angle: 0.0 };
        let dev = Develop { detail: nr(50.0, 50.0), ..Develop::default() };
        let zoom = gpu.read_lin(&gpu.develop(&raw, &r, 2048, 1364, &dev, Profile::Standard).unwrap());
        let fit = gpu.read_lin(&gpu.develop(&raw, &r, 512, 341, &dev, Profile::Standard).unwrap());
        let down: Vec<f32> = (0..512 * 341)
            .map(|i| {
                let (x, y) = (i % 512 * 4, i / 512 * 4);
                (0..16).map(|k| luma(&zoom[((y + k / 4) * 2048 + x + k % 4) as usize])).sum::<f32>() / 16.0
            })
            .collect();
        let fit: Vec<f32> = fit.iter().map(luma).collect();
        // 16×16 blocks; relative std (noise grows with signal), flattest by both images.
        let block = |img: &[f32], bx: u32, by: u32| {
            let (m, s) = mean_std((0..256).map(|k| img[((by * 16 + k / 16) * 512 + bx * 16 + k % 16) as usize]));
            s / m.max(1e-4)
        };
        let mut blocks: Vec<(f32, f32)> =
            (0..32 * 21).map(|i| (block(&fit, i % 32, i / 32), block(&down, i % 32, i / 32))).collect();
        blocks.sort_by(|a, b| (a.0 + a.1).total_cmp(&(b.0 + b.1)));
        let (f, z) = blocks[..16].iter().fold((0.0, 0.0), |(f, z), b| (f + b.0, z + b.1));
        eprintln!("flat-patch noise fit {f:.4} vs 100% {z:.4} (sum of 16)");
        assert!((f / z - 1.0).abs() < 0.25, "fit {f} vs 100% {z}");

        // Balanced drag at 2048 px, all tools on: exposure (everything re-runs), sharpening (cached tone), local contrast.
        let fr = whole(w, h);
        let (pw, ph) = crate::photo::Decoded::fit_size(&fr, 2048);
        let (bw, bh) = ((pw as f32 * 0.75) as u32, (ph as f32 * 0.75) as u32);
        let (sw, sh) = crate::photo::Decoded::fit_size(&fr, 512);
        for (name, edit) in [("exposure", 0), ("sharpen", 1), ("local contrast", 2)] {
            let (mut slot, mut small) = (None, None);
            let mut t = std::time::Duration::ZERO;
            for i in 0..11 {
                let mut d = Develop { detail: Detail { dehaze: 30.0, ..all_tools() }, ..Develop::default() };
                match edit {
                    0 => d.exposure = i as f32 * 0.05,
                    1 => d.detail.sharpen = 40.0 + i as f32,
                    _ => d.detail.local_contrast = 40.0 + i as f32,
                }
                let t0 = std::time::Instant::now();
                gpu.develop_into(&mut slot, &raw, &fr, bw, bh, &d, Profile::Standard).unwrap();
                gpu.develop_into(&mut small, &raw, &fr, sw, sh, &d, Profile::Standard).unwrap();
                gpu.read_photo(small.as_ref().unwrap());
                if i > 0 {
                    t += t0.elapsed()
                } // first frame warms pipelines and caches
            }
            eprintln!("drag {name} {bw}×{bh}: {:?}", t / 10);
        }
    }

    /// On a real file (RAWVALLEY_RAW): each detail stage alone, fit at 1536 px
    /// and a 100% 2048 px view, GPU time per frame (run with --test-threads=1).
    #[test]
    fn stage_cost_on_sample() {
        let Ok(path) = std::env::var("RAWVALLEY_RAW") else { return eprintln!("no sample, skipped") };
        let Ok(gpu) = Gpu::new() else { return eprintln!("no GPU, skipped") };
        let raw = gpu.load_raw(&read_raw(Path::new(&path)).unwrap()).unwrap();
        let (w, h) = raw.oriented_size();
        let fr = whole(w, h);
        let view = Region { center: fr.center, size: [2048.0, 1365.0], angle: 0.0 };
        let tools: [(&str, Detail); 8] = [
            ("none", Detail::default()),
            ("nr_luma", Detail { nr_luma: 50.0, ..Detail::default() }),
            ("nr_chroma", Detail { nr_chroma: 50.0, ..Detail::default() }),
            ("defringe", Detail { defringe: 50.0, ..Detail::default() }),
            ("dehaze", Detail { dehaze: 30.0, ..Detail::default() }),
            ("lc", Detail { local_contrast: 50.0, ..Detail::default() }),
            ("sharpen", Detail { sharpen: 50.0, ..Detail::default() }),
            ("sharpen100", Detail { sharpen: 100.0, ..Detail::default() }),
        ];
        for (vn, r, ow, oh) in [("fit1536", fr, 1536u32, 1023u32), ("100%", view, 2048, 1365)] {
            for (name, d) in tools {
                let mut slot = None;
                let mut t = std::time::Duration::ZERO;
                for i in 0..6 {
                    let dev = Develop { detail: d, exposure: i as f32 * 0.01, ..Develop::default() };
                    let t0 = std::time::Instant::now();
                    gpu.develop_into(&mut slot, &raw, &r, ow, oh, &dev, Profile::Standard).unwrap();
                    gpu.read_range(&slot.as_ref().unwrap().src, 0, 16);
                    if i > 0 {
                        t += t0.elapsed()
                    }
                }
                eprintln!("{vn} {name}: {:?}", t / 5);
            }
        }
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
