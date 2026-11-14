//! Photo loading: JPEG/PNG/TIFF via `image`; camera RAWs via `rawler`, as a
//! small CPU copy here and at full resolution on the GPU (`raw`). What the
//! grade sees is always a camera signal (Standard or log).

use std::path::Path;
use std::sync::Arc;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, Rgba32FImage};
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
pub use rawler::imgop::matrix::multiply_row1;
pub use rawler::imgop::srgb::{srgb_apply_gamma, srgb_invert_gamma};
use rayon::prelude::*;

use crate::develop::{Camera, Develop, develop_pixel};
use crate::profiles::Profile;
use crate::view::{Pyramid, Region};

pub const PREVIEW_MAX: u32 = 2048;
pub const THUMB_MAX: u32 = 256;

pub const RASTER_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff"];
pub const RAW_EXTS: &[&str] = &[
    "rw2", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "dng", "pef", "srw", "iiq", "3fr", "erf",
    "kdc", "mrw", "rwl",
];

#[derive(Clone)]
pub struct Photo {
    pub width: u32,
    pub height: u32,
    /// Display-encoded RGBA in 0..1.
    pub pixels: Vec<[f32; 4]>,
}

impl std::fmt::Debug for Photo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Photo({}×{})", self.width, self.height)
    }
}

pub fn is_raw(path: &Path) -> bool {
    path.extension().is_some_and(|e| RAW_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// A file RawValley opens as a photo (raster or camera RAW).
pub fn is_photo(path: &Path) -> bool {
    is_raw(path) || path.extension().is_some_and(|e| RASTER_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// A decoded file, before it is expressed as a camera signal.
#[derive(Clone, Debug)]
pub enum Decoded {
    /// JPEG/PNG/TIFF: already a camera signal (Standard, or e.g. a V-Log JPEG).
    Raster(Arc<Pyramid>),
    /// RAW: the full resolution lives on the GPU (`raw::GpuRaw`); this is a
    /// small as-shot-balanced camera-RGB copy for CPU uses (filmstrip, match),
    /// the camera model, and the full oriented size.
    Raw { small: Arc<Pyramid>, camera: Arc<Camera>, size: (u32, u32) },
}

impl Decoded {
    /// Full resolution (the photo being edited).
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_max(path, None)
    }

    /// Fitting `max` × `max` when given, without ever holding the full
    /// resolution in RGBA (filmstrip photos, match references). RAWs are
    /// always the small copy (`RAW_SMALL` when `max` is not given).
    pub fn load_max(path: &Path, max: Option<u32>) -> Result<Self, String> {
        if is_raw(path) {
            load_raw_small(path, max.unwrap_or(RAW_SMALL))
        } else {
            load_raster(path, max).map(|p| Decoded::Raster(Arc::new(Pyramid::new(p))))
        }
    }

    /// A RAW developed on the GPU: its small camera-RGB copy, camera and full oriented size.
    pub fn raw(small: Photo, camera: Camera, size: (u32, u32)) -> Self {
        Decoded::Raw { small: Arc::new(Pyramid::new(small)), camera: Arc::new(camera), size }
    }

    fn pyramid(&self) -> &Pyramid {
        match self {
            Decoded::Raster(p) | Decoded::Raw { small: p, .. } => p,
        }
    }

    /// Same variant holding `photo`.
    fn with(&self, photo: Photo) -> Self {
        let p = Arc::new(Pyramid::new(photo));
        match self {
            Decoded::Raster(_) => Decoded::Raster(p),
            Decoded::Raw { camera, size, .. } => Decoded::Raw { small: p, camera: camera.clone(), size: *size },
        }
    }

    /// Full-resolution size of the oriented image.
    pub fn size(&self) -> (u32, u32) {
        match self {
            Decoded::Raster(p) => (p.full().width, p.full().height),
            Decoded::Raw { size, .. } => *size,
        }
    }

    /// The camera model of a RAW.
    pub fn camera(&self) -> Option<&Camera> {
        match self {
            Decoded::Raw { camera, .. } => Some(camera),
            Decoded::Raster(_) => None,
        }
    }

    pub fn is_raw(&self) -> bool {
        matches!(self, Decoded::Raw { .. })
    }

    /// Box-filtered copy that fits in `max` × `max` (filmstrip photos).
    pub fn downscaled(&self, max: u32) -> Self {
        let (w, h) = self.size();
        if w.max(h) <= max {
            return self.clone();
        }
        self.with(self.pyramid().full().downscaled(max))
    }

    /// Output size of `r` fitted within `max` × `max` (never enlarged).
    pub fn fit_size(r: &Region, max: u32) -> (u32, u32) {
        let k = (max as f32 / r.size[0].max(r.size[1])).min(1.0);
        let [w, h] = r.size.map(|v| ((v * k).round() as u32).max(1));
        (w, h)
    }

    /// `r` fitted within `max` × `max` (never enlarged) as `profile`'s signal.
    pub fn encode_fit(&self, r: &Region, max: u32, profile: Profile, dev: &Develop) -> Arc<Photo> {
        let (w, h) = Self::fit_size(r, max);
        self.encode_region(r, w, h, profile, dev)
    }

    /// `r` (full-resolution coordinates) resampled to `out_w` × `out_h` as
    /// `profile`'s signal. RAWs come from the small copy here; the editor
    /// develops them on the GPU instead.
    pub fn encode_region(&self, r: &Region, out_w: u32, out_h: u32, profile: Profile, dev: &Develop) -> Arc<Photo> {
        match self {
            Decoded::Raster(p) => Arc::new(p.resample(r, out_w, out_h)),
            Decoded::Raw { small, camera, size } => {
                let k = small.full().width as f32 / size.0 as f32;
                let r = Region { center: r.center.map(|v| v * k), size: r.size.map(|v| v * k), angle: r.angle };
                Arc::new(develop_photo(small.resample(&r, out_w, out_h), camera, dev, profile))
            }
        }
    }

    /// The pixels as `profile`'s signal: RAWs are rendered into it, rasters
    /// are taken as already encoded in it.
    pub fn encode(&self, profile: Profile, dev: &Develop) -> Arc<Photo> {
        match self {
            Decoded::Raster(p) => Arc::new(p.full().clone()),
            Decoded::Raw { small, camera, .. } => Arc::new(develop_photo(small.full().clone(), camera, dev, profile)),
        }
    }
}

/// Camera RGB as `profile`'s signal, in place (the CPU twin of the GPU develop pass).
fn develop_photo(mut cam: Photo, camera: &Camera, dev: &Develop, profile: Profile) -> Photo {
    cam.pixels.par_iter_mut().for_each(|p| {
        let [r, g, b] = develop_pixel([p[0], p[1], p[2]], camera, dev, profile);
        *p = [r, g, b, 1.0];
    });
    cam
}

impl Photo {
    /// Box-filtered copy that fits in `max` × `max`. Averages the stored
    /// values directly, so linear RAW data is downscaled in linear light.
    // ponytail: not image::imageops::thumbnail — it washes out f32 pixels.
    pub fn downscaled(&self, max: u32) -> Self {
        box_downscale(&self.pixels, self.width, self.height, max, |p| *p)
    }

    fn into_image(self) -> Rgba32FImage {
        Rgba32FImage::from_raw(self.width, self.height, bytemuck::allocation::cast_vec(self.pixels))
            .expect("pixel count matches dimensions")
    }

    pub(crate) fn from_image(img: Rgba32FImage) -> Self {
        Self { width: img.width(), height: img.height(), pixels: bytemuck::allocation::cast_vec(img.into_raw()) }
    }
}

/// Integer-factor box filter over any pixel type convertible to RGBA.
/// Output rows run in parallel.
fn box_downscale<T: Sync>(src: &[T], width: u32, height: u32, max: u32, rgba: impl Fn(&T) -> [f32; 4] + Sync) -> Photo {
    let f = width.max(height).div_ceil(max).max(1);
    let (w, h) = ((width / f).max(1), (height / f).max(1));
    let norm = 1.0 / (f * f) as f32;
    let rgba = &rgba;
    let pixels = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..w).map(move |x| {
                let mut acc = [0.0f32; 4];
                let (rows, cols) = (f.min(height - y * f), f.min(width - x * f));
                for dy in 0..rows {
                    let row = ((y * f + dy) * width + x * f) as usize;
                    for p in &src[row..row + cols as usize] {
                        let p = rgba(p);
                        for c in 0..4 {
                            acc[c] += p[c];
                        }
                    }
                }
                acc.map(|v| v * norm)
            })
        })
        .collect();
    Photo { width: w, height: h, pixels }
}

fn load_raster(path: &Path, max: Option<u32>) -> Result<Photo, String> {
    let mut dec = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = dec.orientation().map_err(|e| e.to_string())?;
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    if let Some(max) = max {
        img = img.thumbnail(max, max);
    }
    let img = img.into_rgba32f();
    let (width, height) = img.dimensions();
    let pixels = img.pixels().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    Ok(Photo { width, height, pixels })
}

/// The JPEG the camera embedded in a RAW, display-encoded, fitting 1024 px.
pub fn embedded_preview(path: &Path) -> Result<Photo, String> {
    let src = rawler::rawsource::RawSource::new(path).map_err(|e| e.to_string())?;
    let params = rawler::decoders::RawDecodeParams::default();
    let decoder = rawler::get_decoder(&src).map_err(|e| e.to_string())?;
    // Not `analyze::extract_preview_pixels`: without a preview it develops the RAW itself.
    let img = decoder.preview_image(&src, &params).map_err(|e| e.to_string())?.ok_or("no camera JPEG")?;
    let img = img.into_rgba32f();
    let (w, h) = img.dimensions();
    Ok(Photo { width: w, height: h, pixels: bytemuck::allocation::cast_vec(img.into_raw()) }.downscaled(1024))
}

/// Longest side of a RAW's small CPU copy when no size is asked for.
pub const RAW_SMALL: u32 = 1024;

/// A RAW's small copy decoded on the CPU (rawler's demosaic): as-shot-balanced
/// camera RGB fitting `max` × `max`, clipped highlights faded to neutral.
// shortcut: filmstrip photos and match references skip the GPU (RCD, inpaint opposed); route them through
// `Gpu::load_raw` + `Gpu::small` if their highlights ever look wrong at strip size.
fn load_raw_small(path: &Path, max: u32) -> Result<Decoded, String> {
    let src = rawler::rawsource::RawSource::new(path).map_err(|e| e.to_string())?;
    let params = rawler::decoders::RawDecodeParams::default();
    let raw = rawler::decode(&src, &params).map_err(|e| e.to_string())?;
    // Decoders such as Panasonic's leave `raw.orientation` at Normal; the EXIF
    // Orientation tag is the reliable source (6/8 = portrait shots).
    let exif_orientation = rawler::get_decoder(&src)
        .and_then(|d| d.raw_metadata(&src, &params))
        .ok()
        .and_then(|m| m.exif.orientation)
        .unwrap_or_else(|| raw.orientation.to_u16());
    use ProcessingStep::*;
    let dev = RawDevelop::new_with(&[Rescale, Demosaic, FujiRotate, CropActiveArea, CropDefault]);
    let mut camera = crate::raw::camera(&raw);
    let g = camera.as_shot;
    let (small, w, h) = match dev.develop_intermediate(&raw).map_err(|e| e.to_string())? {
        Intermediate::ThreeColor(px) => {
            let rgba = |p: &[f32; 3]| balanced(std::array::from_fn(|c| p[c] * g[c]));
            (box_downscale(&px.data, px.width as u32, px.height as u32, max, rgba), px.width, px.height)
        }
        Intermediate::FourColor(px) => {
            camera = Camera::neutral();
            let rgba = |p: &[f32; 4]| balanced([p[0], 0.5 * (p[1] + p[3]), p[2]]);
            (box_downscale(&px.data, px.width as u32, px.height as u32, max, rgba), px.width, px.height)
        }
        Intermediate::Monochrome(_) => return Err("unsupported sensor layout".into()),
    };
    let orientation = Orientation::from_exif(exif_orientation as u8).unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::ImageRgba32F(small.into_image());
    img.apply_orientation(orientation);
    let quarter = matches!(exif_orientation, 5..=8);
    let size = if quarter { (h as u32, w as u32) } else { (w as u32, h as u32) };
    Ok(Decoded::raw(Photo::from_image(img.into_rgba32f()), camera, size))
}

/// A balanced camera colour with clipped highlights faded to neutral: a
/// clipped channel has lost its true value, so its hue is wrong (white
/// balance turns clipped skies magenta). The fade covers the last 15% before
/// clip, wide enough that no edge appears (5% shows one).
fn balanced(cam: [f32; 3]) -> [f32; 4] {
    let white = cam.iter().copied().fold(0.0, f32::max);
    let t = ((white - 0.85) / 0.15).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    let [r, g, b] = cam.map(|v| (v + (white - v) * t).max(0.0));
    [r, g, b, 1.0]
}

/// Scene-linear scale of a developed RAW: Lumix RAWs sit ~0.8 EV under scene grey.
pub(crate) const RAW_TO_SCENE: f32 = 1.76;
const VIEW_EXPOSURE: f32 = 1.25;
const VIEW_WHITE: f32 = 6.0; // exposed scene value mapped to display white

/// Neutral scene → display rendering (linear Rec.709 in, sRGB out):
/// hue-preserving extended Reinhard shoulder. `grade.wgsl` mirrors it for
/// V-Log input, so RAW and V-Log start from the same look.
// ponytail: approximates, does not replicate, the camera's Standard profile.
// For a pixel-exact LUT match shoot RAW+JPEG and grade the JPEG.
pub fn view_transform(lin: [f32; 3]) -> [f32; 3] {
    let c = lin.map(|v| v.max(0.0) * VIEW_EXPOSURE);
    let m = c[0].max(c[1]).max(c[2]);
    let k = if m > 0.0 { (1.0 + m / (VIEW_WHITE * VIEW_WHITE)) / (1.0 + m) } else { 0.0 };
    c.map(|v| srgb_apply_gamma((v * k).min(1.0)))
}

/// Exact inverse of [`view_transform`] before its sRGB step: display-linear
/// Rec.709 in, scene-linear out (the shoulder's quadratic solved for m).
pub fn inverse_view_transform(display_linear: [f32; 3]) -> [f32; 3] {
    let y = display_linear.iter().copied().fold(0.0f32, f32::max).min(1.0);
    if y <= 0.0 {
        return [0.0; 3];
    }
    let w2 = VIEW_WHITE * VIEW_WHITE;
    // m²/W² + m(1 − y) − y = 0
    let m = (-(1.0 - y) + ((1.0 - y) * (1.0 - y) + 4.0 * y / w2).sqrt()) * w2 / 2.0;
    display_linear.map(|v| v.max(0.0) / y * m / VIEW_EXPOSURE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clipped colour comes out neutral, and the fade has no step.
    #[test]
    fn clipped_highlights_are_neutral_and_smooth() {
        let [r, g, b, _] = balanced([0.8, 1.0, 0.85]);
        assert!(r == g && g == b, "{r} {g} {b}");
        let (lo, hi) = (balanced([0.3, 0.8499, 0.4]), balanced([0.3, 0.8501, 0.4]));
        assert!((0..3).all(|c| (lo[c] - hi[c]).abs() < 1e-3), "{lo:?} {hi:?}");
    }

    /// The camera JPEG of a sample RAW (RAWVALLEY_RAW), when present.
    #[test]
    fn embedded_preview_of_sample_raw() {
        let Ok(path) = std::env::var("RAWVALLEY_RAW") else { return eprintln!("no sample, skipped") };
        let p = embedded_preview(Path::new(&path)).unwrap();
        assert!((640..=1024).contains(&p.width.max(p.height)), "{}×{}", p.width, p.height);
        assert!(p.pixels.iter().all(|c| c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))));
    }

    /// Portrait RAWs must come out portrait. Uses local sample files when
    /// present (set RAWVALLEY_PORTRAIT_RAW), otherwise skips.
    #[test]
    fn portrait_raw_is_rotated() {
        let Ok(path) = std::env::var("RAWVALLEY_PORTRAIT_RAW") else { return eprintln!("no sample, skipped") };
        let d = Decoded::load(Path::new(&path)).unwrap();
        assert!(d.is_raw(), "not a RAW");
        let (w, h) = d.size();
        assert!(h > w, "{w}×{h} is landscape");
    }
}
