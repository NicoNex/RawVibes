//! Photo export: the crop at full resolution, developed and graded in
//! horizontal strips (so any photo fits the GPU's buffer limits), then
//! encoded as JPEG, 16-bit PNG or 16-bit TIFF (display sRGB).

use std::io::{BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::{ExtendedColorType, ImageEncoder};

use rayon::prelude::*;

use crate::meta::Tags;
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

/// The compact sRGB v2 profile (saucecontrol/Compact-ICC-Profiles, CC0): what the export's pixels are in.
pub const SRGB_ICC: &[u8] = include_bytes!("srgb.icc");

/// `v` as 8 bits with triangular (TPDF) dither of ±1 level, so smooth gradients do not band.
/// The noise narrows towards black and white, where clipping would shift the mean.
fn to_u8_dithered(v: u16, i: usize) -> u8 {
    // SplitMix64 of the sample index: its two 24-bit halves are two uniforms in [0, 1).
    let mut z = (i as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    let unit = |bits: u64| (bits & 0xff_ffff) as f32 / (1 << 24) as f32;
    let x = v as f32 * 255.0 / 65535.0;
    let amp = x.min(255.0 - x).min(1.0);
    let noise = unit(z >> 40) - unit(z >> 16);
    (x + amp * noise).round().clamp(0.0, 255.0) as u8
}

/// Writes `rgb` (16-bit, `w` × `h`) to `path`. Through a `.part` file
/// renamed at the end, so a failed export never destroys the file it
/// replaces; not renamed (`Ok(false)`) once `cancelled` says so.
#[allow(clippy::too_many_arguments)]
pub fn save(
    path: &Path,
    w: u32,
    h: u32,
    rgb: Vec<u16>,
    format: PhotoFormat,
    quality: u8,
    tags: &Tags,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let write = || -> Result<(), String> {
        let mut file = BufWriter::new(std::fs::File::create(&part).map_err(|e| e.to_string())?);
        if rgb.len() != w as usize * h as usize * 3 {
            return Err("export buffer size mismatch".into());
        }
        let px: &[u8] = bytemuck::cast_slice(&rgb);
        let err = |e: image::ImageError| e.to_string();
        match format {
            PhotoFormat::Jpeg => {
                if let Some(xmp) = &tags.xmp {
                    // XMP has no encoder hook: encoded in memory, the segment written in after the APPn ones.
                    let mut jpeg = Vec::new();
                    encode_jpeg(&mut jpeg, &rgb, w, h, quality, &tags.exif)?;
                    let (head, tail) = jpeg.split_at(after_app_segments(&jpeg));
                    let segment = app1(&[XMP_NS, xmp.as_bytes()].concat());
                    [head, &segment, tail].iter().try_for_each(|b| file.write_all(b)).map_err(|e| e.to_string())?;
                } else {
                    encode_jpeg(&mut file, &rgb, w, h, quality, &tags.exif)?;
                }
            }
            PhotoFormat::Png16 => {
                let mut enc = PngEncoder::new(&mut file);
                let _ = enc.set_icc_profile(SRGB_ICC.to_vec());
                if !tags.exif.is_empty() {
                    let _ = enc.set_exif_metadata(tags.exif.clone());
                }
                enc.write_image(px, w, h, ExtendedColorType::Rgb16).map_err(err)?;
                if let Some(x) = &tags.xmp {
                    // iTXt may follow the image data: written over IEND (the last 12 bytes), then IEND again.
                    file.seek(SeekFrom::End(-12)).map_err(|e| e.to_string())?;
                    let itxt = [&b"XML:com.adobe.xmp\0\0\0\0\0"[..], x.as_bytes()].concat();
                    file.write_all(&png_chunk(b"iTXt", &itxt)).map_err(|e| e.to_string())?;
                    file.write_all(&png_chunk(b"IEND", &[])).map_err(|e| e.to_string())?;
                }
            }
            PhotoFormat::Tiff16 => {
                // shortcut: TIFF gets the ICC profile only, no EXIF/XMP; add them as custom tags when asked.
                let mut enc = TiffEncoder::new(&mut file);
                let _ = enc.set_icc_profile(SRGB_ICC.to_vec());
                enc.write_image(px, w, h, ExtendedColorType::Rgb16).map_err(err)?;
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
    let _ = encode_jpeg(&mut out, rgb, w, h, quality, &[]);
    out.into_inner().len()
}

fn encode_jpeg(to: impl Write, rgb: &[u16], w: u32, h: u32, quality: u8, exif: &[u8]) -> Result<(), String> {
    let px: Vec<u8> = rgb.par_iter().enumerate().map(|(i, &v)| to_u8_dithered(v, i)).collect();
    let mut enc = JpegEncoder::new_with_quality(to, quality);
    let _ = enc.set_icc_profile(SRGB_ICC.to_vec());
    if !exif.is_empty() {
        let _ = enc.set_exif_metadata(exif.to_vec());
    }
    enc.encode(&px, w, h, ExtendedColorType::Rgb8).map_err(|e| e.to_string())
}

/// The namespace that opens a JPEG's XMP segment.
const XMP_NS: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// Where the segments after a JPEG's APPn ones start: XMP goes there, after EXIF and ICC.
fn after_app_segments(jpeg: &[u8]) -> usize {
    let mut at = 2;
    while let (Some(0xff), Some(0xe0..=0xef), Some(&a), Some(&b)) =
        (jpeg.get(at), jpeg.get(at + 1), jpeg.get(at + 2), jpeg.get(at + 3))
    {
        at += 2 + u16::from_be_bytes([a, b]) as usize;
    }
    at.min(jpeg.len())
}

/// Whether `xmp` fits a JPEG's one XMP segment; a JPEG export leaves out a packet that does not.
pub fn xmp_fits_jpeg(xmp: &str) -> bool {
    2 + XMP_NS.len() + xmp.len() <= u16::MAX as usize
}

/// An APP1 segment of `payload`; empty when too big for one (the export goes without it).
fn app1(payload: &[u8]) -> Vec<u8> {
    let Ok(len) = u16::try_from(payload.len() + 2) else { return Vec::new() };
    [&[0xff, 0xe1][..], &len.to_be_bytes(), payload].concat()
}

/// A PNG chunk: length, type, data, CRC-32 of type and data.
fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut crc = !0u32;
    for &byte in kind.iter().chain(data) {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    [&(data.len() as u32).to_be_bytes()[..], kind, data, &(!crc).to_be_bytes()].concat()
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
            assert!(save(&p, w, h, rgb.clone(), f, JPEG_QUALITY, &Tags::default(), || false).unwrap());
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
        assert!(!save(&p, w, h, vec![0; rgb.len()], PhotoFormat::Png16, 90, &Tags::default(), || true).unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(!dir.join("x.png.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dither_keeps_the_mean() {
        for v in [0u16, 1000, 32768, 65535] {
            let n = 100_000;
            let px: Vec<u8> = (0..n).map(|i| to_u8_dithered(v, i)).collect();
            let mean = px.iter().map(|&p| p as f64).sum::<f64>() / n as f64;
            assert!((mean - v as f64 * 255.0 / 65535.0).abs() < 0.02, "{v}: {mean}");
            assert!(px.iter().all(|&p| if v == 0 {
                p <= 1
            } else if v == 65535 {
                p >= 254
            } else {
                true
            }));
        }
        // Not a constant rounding: mid-grey between two levels takes both.
        assert!((0..100).map(|i| to_u8_dithered(32896, i)).collect::<std::collections::HashSet<_>>().len() > 1);
    }

    fn sample_tags(w: u32, h: u32) -> Tags {
        let src = rawler::decoders::RawMetadata {
            exif: rawler::exif::Exif { iso_speed_ratings: Some(400), ..Default::default() },
            make: "Fujifilm".into(),
            model: "X-T5".into(),
            ..Default::default()
        };
        let m = crate::sidecar::Meta { creator: "Ada".into(), title: "Alba".into(), ..Default::default() };
        crate::meta::tags(Some(&src), &m, crate::meta::Policy::Essential, (w, h))
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn jpeg_and_png_carry_metadata() {
        let (w, h) = (16, 8);
        let rgb: Vec<u16> = (0..w * h * 3).map(|i| (i * 300) as u16).collect();
        let dir = std::env::temp_dir().join(format!("rawvibes-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (f, marks) in [
            (PhotoFormat::Jpeg, [&b"Exif\0\0"[..], b"ICC_PROFILE", b"http://ns.adobe.com/xap/1.0/\0"]),
            (PhotoFormat::Png16, [&b"eXIf"[..], b"iCCP", b"iTXtXML:com.adobe.xmp"]),
        ] {
            let p = dir.join(format!("m.{}", f.ext()));
            assert!(save(&p, w, h, rgb.clone(), f, 90, &sample_tags(w, h), || false).unwrap());
            let bytes = std::fs::read(&p).unwrap();
            for m in marks {
                assert!(contains(&bytes, m), "{f:?} lacks {}", String::from_utf8_lossy(m));
            }
            assert!(contains(&bytes, b"Alba"));
            let back = image::load_from_memory(&bytes).unwrap();
            assert_eq!((back.width(), back.height()), (w, h));
        }
        // RawVibes reads its own JPEG's EXIF back.
        let back = crate::meta::read(&dir.join("m.jpg")).expect("EXIF");
        assert_eq!((back.make.as_str(), back.exif.iso_speed_ratings), ("Fujifilm", Some(400)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tiff_carries_the_profile() {
        let dir = std::env::temp_dir().join(format!("rawvibes-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("i.tif");
        assert!(save(&p, 4, 4, vec![1000; 48], PhotoFormat::Tiff16, 90, &Tags::default(), || false).unwrap());
        assert!(contains(&std::fs::read(&p).unwrap(), &SRGB_ICC[..40]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One APP1 segment holds 65535 bytes, length and namespace included.
    #[test]
    fn xmp_fits_one_segment() {
        let limit = 65535 - 2 - XMP_NS.len();
        assert!(xmp_fits_jpeg(&"x".repeat(limit)));
        assert!(!xmp_fits_jpeg(&"x".repeat(limit + 1)));
        assert!(app1(&[XMP_NS, "x".repeat(limit + 1).as_bytes()].concat()).is_empty());
    }

    #[test]
    fn jpeg_size_grows_with_quality() {
        let rgb: Vec<u16> = (0..64 * 64 * 3).map(|i| (i * 37 % 251 * 257) as u16).collect();
        assert!(jpeg_size(&rgb, 64, 64, 50) < jpeg_size(&rgb, 64, 64, 95));
    }
}
