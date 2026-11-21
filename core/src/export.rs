//! Photo export: the crop at full resolution, developed and graded in
//! horizontal strips (so any photo fits the GPU's buffer limits), then
//! encoded as JPEG, 16-bit PNG or 16-bit TIFF (display sRGB).

use std::io::{BufWriter, Cursor, Write};
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageBuffer, ImageFormat, Rgb};

use crate::view::Region;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoFormat {
    Jpeg,
    Png16,
    Tiff16,
}

impl PhotoFormat {
    pub const ALL: [PhotoFormat; 3] = [PhotoFormat::Jpeg, PhotoFormat::Png16, PhotoFormat::Tiff16];

    pub fn ext(self) -> &'static str {
        match self {
            PhotoFormat::Jpeg => "jpg",
            PhotoFormat::Png16 => "png",
            PhotoFormat::Tiff16 => "tif",
        }
    }

    /// Settings key word, also the menu action target.
    pub fn tag(self) -> &'static str {
        match self {
            PhotoFormat::Jpeg => "jpeg",
            PhotoFormat::Png16 => "png16",
            PhotoFormat::Tiff16 => "tiff16",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.tag() == tag)
    }
}

/// Default JPEG quality (as Lightroom's export: high, without the size of 100).
pub const JPEG_QUALITY: u8 = 90;

/// Rows `y0..y1` of the `h`-row full-resolution output of `crop`, as a region
/// of the same width and angle (vignette and resampling stay continuous).
pub fn strip(crop: &Region, h: u32, y0: u32, y1: u32) -> Region {
    let sy = crop.size[1] / h as f32;
    let mid = (y0 + y1) as f32 / 2.0 - h as f32 / 2.0;
    Region { center: crop.point([0.0, mid * sy]), size: [crop.size[0], (y1 - y0) as f32 * sy], angle: crop.angle }
}

fn to_u8(v: u16) -> u8 {
    ((v as u32 * 255 + 32767) / 65535) as u8
}

/// Writes `rgb` (16-bit, `w` × `h`) to `path`. Through a `.part` file
/// renamed at the end, so a failed export never destroys the file it
/// replaces; not renamed (`Ok(false)`) once `cancelled` says so.
pub fn save(
    path: &Path,
    w: u32,
    h: u32,
    rgb: Vec<u16>,
    format: PhotoFormat,
    quality: u8,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let write = || -> Result<(), String> {
        let mut file = BufWriter::new(std::fs::File::create(&part).map_err(|e| e.to_string())?);
        match format {
            PhotoFormat::Jpeg => encode_jpeg(&mut file, &rgb, w, h, quality)?,
            PhotoFormat::Png16 | PhotoFormat::Tiff16 => {
                let img = ImageBuffer::<Rgb<u16>, _>::from_raw(w, h, rgb).ok_or("export buffer size mismatch")?;
                let f = if format == PhotoFormat::Png16 { ImageFormat::Png } else { ImageFormat::Tiff };
                img.write_to(&mut file, f).map_err(|e| e.to_string())?;
            }
        }
        // The last buffered bytes and the disk sync can fail too (disk full): before the rename.
        file.into_inner().map_err(|e| e.error().to_string())?.sync_all().map_err(|e| e.to_string())
    };
    let r = write().and_then(|_| {
        if cancelled() {
            return Ok(false);
        }
        std::fs::rename(&part, path).map(|_| true).map_err(|e| e.to_string())
    });
    if r != Ok(true) {
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// Bytes of a 16-bit RGB image (`w` × `h`) as a JPEG at `quality`: what the
/// export dialog scales by the pixel-count ratio to estimate the file size.
pub fn jpeg_size(rgb: &[u16], w: u32, h: u32, quality: u8) -> usize {
    let mut out = Cursor::new(Vec::new());
    let _ = encode_jpeg(&mut out, rgb, w, h, quality);
    out.into_inner().len()
}

fn encode_jpeg(to: impl Write, rgb: &[u16], w: u32, h: u32, quality: u8) -> Result<(), String> {
    let px: Vec<u8> = rgb.iter().map(|&v| to_u8(v)).collect();
    JpegEncoder::new_with_quality(to, quality).encode(&px, w, h, ExtendedColorType::Rgb8).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tile_the_crop() {
        let crop = Region { center: [500.0, 400.0], size: [600.0, 300.0], angle: 7.0 };
        let (a, b) = (strip(&crop, 300, 0, 100), strip(&crop, 300, 100, 300));
        assert_eq!(strip(&crop, 300, 0, 300), crop);
        // Each strip's top edge is the previous one's bottom edge.
        let bottom = a.point([0.0, a.size[1] / 2.0]);
        let top = b.point([0.0, -b.size[1] / 2.0]);
        assert!((bottom[0] - top[0]).abs() < 1e-3 && (bottom[1] - top[1]).abs() < 1e-3);
        assert_eq!(a.size[1] + b.size[1], crop.size[1]);
    }

    #[test]
    fn formats_round_trip() {
        let (w, h) = (8, 4);
        let rgb: Vec<u16> = (0..w * h * 3).map(|i| (i * 2000) as u16).collect();
        let dir = std::env::temp_dir().join(format!("rawvibes-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in PhotoFormat::ALL {
            let p = dir.join(format!("x.{}", f.ext()));
            assert!(save(&p, w, h, rgb.clone(), f, JPEG_QUALITY, || false).unwrap());
            let back = image::open(&p).unwrap();
            assert_eq!((back.width(), back.height()), (w, h));
            if f != PhotoFormat::Jpeg {
                assert_eq!(back.into_rgb16().into_raw(), rgb, "{f:?} is lossless 16-bit");
            }
            assert!(!dir.join(format!("x.{}.part", f.ext())).exists());
        }
        // Stopped while saving: the file it would replace is untouched, no `.part` left.
        let p = dir.join("x.png");
        let before = std::fs::read(&p).unwrap();
        assert!(!save(&p, w, h, vec![0; rgb.len()], PhotoFormat::Png16, 90, || true).unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(!dir.join("x.png.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn jpeg_size_grows_with_quality() {
        let rgb: Vec<u16> = (0..64 * 64 * 3).map(|i| (i * 37 % 251 * 257) as u16).collect();
        assert!(jpeg_size(&rgb, 64, 64, 50) < jpeg_size(&rgb, 64, 64, 95));
    }
}
