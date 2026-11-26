//! Photo export: the crop at full resolution, developed and graded in
//! horizontal strips (so any photo fits the GPU's buffer limits), then
//! encoded as JPEG, 16-bit PNG or 16-bit TIFF (display sRGB), or with its HDR
//! rendition: an Ultra HDR JPEG (the SDR JPEG plus an ISO 21496-1 gain map), or
//! JPEG XL, AVIF and 16-bit PNG in Rec.2100 PQ.

use std::io::{BufWriter, Cursor, Seek, Write};
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::tiff::TiffEncoder;
use image::{ExtendedColorType, ImageEncoder};

use rayon::prelude::*;

use crate::meta::Tags;
use crate::view::Region;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoFormat {
    Jpeg,
    /// JPEG with a gain map (Ultra HDR v1 XMP and ISO 21496-1): SDR where HDR is not shown.
    UltraHdr,
    /// JPEG XL in Rec.2100 PQ (libjxl); decoders tone-map it for SDR screens.
    JxlHdr,
    /// AVIF in Rec.2100 PQ, 10-bit 4:4:4 (libavif with libaom).
    AvifHdr,
    /// 16-bit PNG in Rec.2100 PQ (cICP), lossless.
    PngHdr,
    Png16,
    Tiff16,
}

impl PhotoFormat {
    pub const ALL: [PhotoFormat; 7] = [
        PhotoFormat::Jpeg,
        PhotoFormat::UltraHdr,
        PhotoFormat::JxlHdr,
        PhotoFormat::AvifHdr,
        PhotoFormat::PngHdr,
        PhotoFormat::Png16,
        PhotoFormat::Tiff16,
    ];

    /// Whether the quality setting applies (lossy).
    pub fn lossy(self) -> bool {
        matches!(self, PhotoFormat::Jpeg | PhotoFormat::UltraHdr | PhotoFormat::JxlHdr | PhotoFormat::AvifHdr)
    }

    /// Whether it carries the HDR rendition (and its headroom applies).
    pub fn hdr(self) -> bool {
        matches!(self, PhotoFormat::UltraHdr | PhotoFormat::JxlHdr | PhotoFormat::AvifHdr | PhotoFormat::PngHdr)
    }

    pub fn ext(self) -> &'static str {
        match self {
            PhotoFormat::Jpeg | PhotoFormat::UltraHdr => "jpg",
            PhotoFormat::JxlHdr => "jxl",
            PhotoFormat::AvifHdr => "avif",
            PhotoFormat::PngHdr | PhotoFormat::Png16 => "png",
            PhotoFormat::Tiff16 => "tif",
        }
    }

    /// Settings key word, also the menu action target.
    pub fn tag(self) -> &'static str {
        match self {
            PhotoFormat::Jpeg => "jpeg",
            PhotoFormat::UltraHdr => "ultrahdr",
            PhotoFormat::JxlHdr => "jxl-hdr",
            PhotoFormat::AvifHdr => "avif-hdr",
            PhotoFormat::PngHdr => "png-hdr",
            PhotoFormat::Png16 => "png16",
            PhotoFormat::Tiff16 => "tiff16",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.tag() == tag)
    }
}

/// Default HDR headroom, in stops over SDR white: 4×, what phones' HDR photos commonly carry.
pub const HDR_HEADROOM: f32 = 2.0;

/// An exported photo: `w` × `h` 16-bit RGB, and for an HDR export each pixel's
/// gain (log2 over `headroom` stops, 16-bit; else empty).
#[derive(Debug)]
pub struct Exported {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u16>,
    pub gains: Vec<u16>,
    pub headroom: f32,
}

impl Exported {
    /// Whether it carries the HDR gains (rendered for an HDR format).
    pub fn hdr(&self) -> bool {
        !self.gains.is_empty()
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

/// Writes `img` to `path`. Through a `.part` file
/// renamed at the end, so a failed export never destroys the file it
/// replaces; not renamed (`Ok(false)`) once `cancelled` says so.
pub fn save(
    path: &Path,
    img: Exported,
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
        encode(&mut file, &img, format, quality, tags)?;
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

/// Bytes of `img` encoded as `format` at `quality`, gain map included: what the
/// export dialog scales by the pixel-count ratio to estimate the file size.
pub fn encoded_size(img: &Exported, format: PhotoFormat, quality: u8) -> usize {
    let mut out = Cursor::new(Vec::new());
    let _ = encode(&mut out, img, format, quality, &Tags::default());
    out.into_inner().len()
}

/// `img` encoded as `format` into `to`.
fn encode(
    to: &mut (impl Write + Seek),
    img: &Exported,
    format: PhotoFormat,
    quality: u8,
    tags: &Tags,
) -> Result<(), String> {
    let Exported { w, h, ref rgb, .. } = *img;
    if rgb.len() != w as usize * h as usize * 3 {
        return Err("export buffer size mismatch".into());
    }
    if format.hdr() && img.gains.len() != w as usize * h as usize {
        return Err("HDR gain buffer size mismatch".into());
    }
    let io = |e: std::io::Error| e.to_string();
    match format {
        PhotoFormat::Jpeg => {
            if let Some(xmp) = &tags.xmp {
                // XMP has no encoder hook: encoded in memory, the segment written in after the APPn ones.
                let mut jpeg = Vec::new();
                encode_jpeg(&mut jpeg, rgb, w, h, quality, &tags.exif)?;
                let (head, tail) = jpeg.split_at(after_app_segments(&jpeg));
                let segment = app1(&[XMP_NS, xmp.as_bytes()].concat());
                [head, &segment, tail].iter().try_for_each(|b| to.write_all(b)).map_err(io)
            } else {
                encode_jpeg(to, rgb, w, h, quality, &tags.exif)
            }
        }
        PhotoFormat::UltraHdr => {
            let mut base = Vec::new();
            encode_jpeg(&mut base, rgb, w, h, quality, &tags.exif)?;
            ultra_hdr(to, &base, img, quality, tags.xmp.as_deref())
        }
        PhotoFormat::JxlHdr => to.write_all(&hdr::jxl(img, quality, tags)?).map_err(io),
        PhotoFormat::AvifHdr => to.write_all(&hdr::avif(img, quality, tags)?).map_err(io),
        PhotoFormat::PngHdr => {
            let pq = hdr::pq_rec2100(img);
            write_png(to, w, h, &pq.samples, Some(pq.light), tags)
        }
        PhotoFormat::Png16 => write_png(to, w, h, rgb, None, tags),
        PhotoFormat::Tiff16 => {
            // shortcut: TIFF gets the ICC profile only, no EXIF/XMP; add them as custom tags when asked.
            let mut enc = TiffEncoder::new(to);
            let _ = enc.set_icc_profile(SRGB_ICC.to_vec());
            enc.write_image(bytemuck::cast_slice(rgb), w, h, ExtendedColorType::Rgb16).map_err(|e| e.to_string())
        }
    }
}

/// A 16-bit RGB PNG with EXIF and XMP: sRGB (ICC), or Rec.2100 PQ (cICP, with
/// its content light level) when `pq` gives the light level.
fn write_png(to: impl Write, w: u32, h: u32, px: &[u16], pq: Option<hdr::Light>, tags: &Tags) -> Result<(), String> {
    let err = |e: png::EncodingError| e.to_string();
    let mut info = png::Info::with_size(w, h);
    (info.color_type, info.bit_depth) = (png::ColorType::Rgb, png::BitDepth::Sixteen);
    if pq.is_none() {
        info.icc_profile = Some(SRGB_ICC.into());
    }
    if !tags.exif.is_empty() {
        info.exif_metadata = Some(tags.exif.as_slice().into());
    }
    let mut enc = png::Encoder::with_info(to, info).map_err(err)?;
    // As fast as the image crate's default: 16-bit photos barely gain from slower deflate.
    enc.set_compression(png::Compression::Fast);
    if let Some(x) = &tags.xmp {
        enc.add_itxt_chunk("XML:com.adobe.xmp".into(), x.clone()).map_err(err)?;
    }
    let mut writer = enc.write_header().map_err(err)?;
    if let Some(light) = pq {
        // The png crate reads but does not write these: Rec.2100 PQ, full range; light in 0.0001 cd/m².
        writer.write_chunk(png::chunk::ChunkType(*b"cICP"), &[9, 16, 0, 1]).map_err(err)?;
        let clli = [light.max_cll, light.max_fall].map(|n| ((n * 10_000.0) as u32).to_be_bytes()).concat();
        writer.write_chunk(png::chunk::ChunkType(*b"cLLI"), &clli).map_err(err)?;
    }
    // Row by row, big-endian: no second copy of the image.
    let mut stream = writer.stream_writer().map_err(err)?;
    let mut row = Vec::with_capacity(w as usize * 6);
    for line in px.chunks_exact(w as usize * 3) {
        row.clear();
        row.extend(line.iter().flat_map(|v| v.to_be_bytes()));
        stream.write_all(&row).map_err(|e| e.to_string())?;
    }
    stream.finish().map_err(err)
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

/// Gain map pixels per side of a photo pixel: half resolution, as Apple's HDR photos
/// (Android writes a quarter); fine enough that the gain does not glow past edges.
const GAIN_MAP_SCALE: u32 = 2;

/// `base` (the SDR JPEG of `img`) made an Ultra HDR JPEG: its gain map, downscaled and
/// encoded at `quality`, appended as an MPF secondary image with ISO 21496-1 and Ultra HDR
/// v1 XMP metadata; the photo's own `xmp` kept, the container directory added to it.
fn ultra_hdr(to: impl Write, base: &[u8], img: &Exported, quality: u8, xmp: Option<&str>) -> Result<(), String> {
    use ultrahdr_rs::metadata::{
        build_gainmap_metadata_markers, create_mpf_header, create_version_only_iso_app2, create_xmp_app1_marker,
        generate_primary_xmp,
    };
    use ultrahdr_rs::{GainMapEncodingFormat, GainMapMetadata};

    let (gw, gh) = (img.w.div_ceil(GAIN_MAP_SCALE), img.h.div_ceil(GAIN_MAP_SCALE));
    // Block mean of the log2 gain (the map is linear in it: gamma 1).
    let map: Vec<u8> = (0..gw * gh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = ((i % gw) * GAIN_MAP_SCALE, (i / gw) * GAIN_MAP_SCALE);
            let (mut sum, mut n) = (0u32, 0u32);
            for y in by..(by + GAIN_MAP_SCALE).min(img.h) {
                for x in bx..(bx + GAIN_MAP_SCALE).min(img.w) {
                    sum += img.gains[(y * img.w + x) as usize] as u32;
                    n += 1;
                }
            }
            (sum as f32 / n as f32 * 255.0 / 65535.0).round() as u8
        })
        .collect();
    let mut gain_jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut gain_jpeg, quality)
        .encode(&map, gw, gh, ExtendedColorType::L8)
        .map_err(|e| e.to_string())?;
    // HDR = SDR × 2^(map × headroom), in linear light: no offsets, so the SDR base is exact.
    let stops = img.headroom as f64;
    let mut metadata = GainMapMetadata::default();
    metadata.gain_map_max = [stops; 3];
    metadata.gain_map_min = [0.0; 3];
    metadata.base_offset = [0.0; 3];
    metadata.alternate_offset = [0.0; 3];
    metadata.alternate_hdr_headroom = stops;
    let gain_markers = build_gainmap_metadata_markers(&metadata, GainMapEncodingFormat::Both).concat();
    let gain_map = [&gain_jpeg[..2], &gain_markers, &gain_jpeg[2..]].concat();

    // The primary: its APPn segments (EXIF, ICC), the ISO version, the XMP, the MPF index, the image.
    let hdr_xmp = generate_primary_xmp(gain_map.len());
    let xmp = match xmp {
        // One packet: the container's rdf:Description beside the photo's.
        Some(x) if x.contains("</rdf:RDF>") => {
            let d = hdr_xmp.find("<rdf:Description").zip(hdr_xmp.find("</rdf:Description>"));
            let (a, b) = d.ok_or("Ultra HDR XMP without a description")?;
            x.replacen("</rdf:RDF>", &format!("{}</rdf:RDF>", &hdr_xmp[a..b + "</rdf:Description>".len()]), 1)
        }
        _ => hdr_xmp.clone(),
    };
    // Too big for one APP1 segment: the container alone, as a plain JPEG drops the packet.
    let xmp = if xmp_fits_jpeg(&xmp) { xmp } else { hdr_xmp };
    let (head, tail) = base.split_at(after_app_segments(base));
    let before_mpf = [head, &create_version_only_iso_app2(), &create_xmp_app1_marker(&xmp)].concat();
    let mpf_len = create_mpf_header(0, 0, Some(before_mpf.len())).len();
    let primary_len = before_mpf.len() + mpf_len + tail.len();
    let mpf = create_mpf_header(primary_len, gain_map.len(), Some(before_mpf.len()));
    let mut to = to;
    [&before_mpf[..], &mpf, tail, &gain_map].iter().try_for_each(|b| to.write_all(b)).map_err(|e| e.to_string())
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

mod hdr;
pub use hdr::{SDR_WHITE, pq_encode};

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
        // The image crate reads back the SDR ones; the HDR ones have their own tests.
        for f in PhotoFormat::ALL.into_iter().filter(|f| !f.hdr() || *f == PhotoFormat::UltraHdr) {
            let p = dir.join(format!("x.{}", f.ext()));
            assert!(save(&p, img(w, h, rgb.clone()), f, JPEG_QUALITY, &Tags::default(), || false).unwrap());
            let back = image::open(&p).unwrap();
            assert_eq!((back.width(), back.height()), (w, h));
            if !f.lossy() {
                assert_eq!(back.into_rgb16().into_raw(), rgb, "{f:?} is lossless 16-bit");
            }
            assert!(!dir.join(format!("x.{}.part", f.ext())).exists());
        }
        // Stopped while saving: the file it would replace is untouched, no `.part` left.
        let p = dir.join("x.png");
        let before = std::fs::read(&p).unwrap();
        assert!(!save(&p, img(w, h, vec![0; rgb.len()]), PhotoFormat::Png16, 90, &Tags::default(), || true).unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(!dir.join("x.png.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An SDR export's pixels, with a neutral HDR gain (2 stops of headroom unused).
    fn img(w: u32, h: u32, rgb: Vec<u16>) -> Exported {
        Exported { w, h, rgb, gains: vec![0; (w * h) as usize], headroom: HDR_HEADROOM }
    }

    #[test]
    fn ultra_hdr_carries_its_gain_map() {
        let (w, h) = (64, 32);
        let rgb: Vec<u16> = (0..w * h * 3).map(|i| (i * 97) as u16).collect();
        // Gain rising left to right, in 2×2 blocks: the half-resolution map holds it exactly.
        let gains = (0..w * h).map(|i| ((i % w) / 2 * 2 * 65535 / (w - 2)).min(65535) as u16).collect();
        let dir = std::env::temp_dir().join(format!("rawvibes-uhdr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("u.jpg");
        let e = Exported { w, h, rgb, gains, headroom: 2.5 };
        assert!(save(&p, e, PhotoFormat::UltraHdr, 95, &sample_tags(w, h), || false).unwrap());
        let bytes = std::fs::read(&p).unwrap();

        let d = ultrahdr_rs::Decoder::new(&bytes).unwrap();
        assert!(d.is_ultrahdr());
        let m = d.metadata().expect("gain map metadata");
        assert_eq!((m.gain_map_min[0], m.gain_map_max[0], m.alternate_hdr_headroom), (0.0, 2.5, 2.5));
        assert_eq!((m.base_offset[0], m.base_hdr_headroom, m.backward_direction), (0.0, 0.0, false));
        // The MPF index finds the map: half resolution, grey, the ramp kept.
        let map = image::load_from_memory(d.gainmap_jpeg().expect("gain map image")).unwrap().into_luma8();
        assert_eq!(map.dimensions(), (w / 2, h / 2));
        let row: Vec<i32> = (0..w / 2).map(|x| map.get_pixel(x, h / 4)[0] as i32).collect();
        for (x, &v) in row.iter().enumerate() {
            let want = (x as u32 * 2 * 255 / (w - 2)) as i32;
            assert!((v - want).abs() <= 3, "map[{x}] = {v}, want {want}");
        }
        // The primary: one XMP packet with the photo's fields and the container, EXIF, ICC.
        let primary = &bytes[..bytes.len() - d.gainmap_jpeg().unwrap().len()];
        assert_eq!(primary.windows(XMP_NS.len()).filter(|w| *w == XMP_NS).count(), 1);
        for mark in [&b"Alba"[..], b"hdrgm:Version", b"Container:Directory", b"Exif\0\0", b"ICC_PROFILE"] {
            assert!(contains(primary, mark), "lacks {}", String::from_utf8_lossy(mark));
        }
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), w);
        assert_eq!(crate::meta::read(&p).expect("EXIF").make, "Fujifilm");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Metadata too long for a JPEG segment: the Ultra HDR file stays valid, the container kept.
    #[test]
    fn ultra_hdr_drops_xmp_too_long_for_a_segment() {
        let tags = Tags {
            exif: Vec::new(),
            xmp: Some(format!("<x:xmpmeta><rdf:RDF>{}</rdf:RDF></x:xmpmeta>", "x".repeat(70_000))),
        };
        let mut out = Cursor::new(Vec::new());
        encode(&mut out, &img(16, 16, vec![30_000; 16 * 16 * 3]), PhotoFormat::UltraHdr, 90, &tags).unwrap();
        let bytes = out.into_inner();
        let d = ultrahdr_rs::Decoder::new(&bytes).unwrap();
        assert!(d.is_ultrahdr() && d.gainmap_jpeg().is_some());
        assert!(!contains(&bytes, b"xxxxxxxx"));
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 16);
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
            assert!(save(&p, img(w, h, rgb.clone()), f, 90, &sample_tags(w, h), || false).unwrap());
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
        assert!(save(&p, img(4, 4, vec![1000; 48]), PhotoFormat::Tiff16, 90, &Tags::default(), || false).unwrap());
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
    fn size_grows_with_quality() {
        let rgb: Vec<u16> = (0..64 * 64 * 3).map(|i| (i * 37 % 251 * 257) as u16).collect();
        let gains = (0..64 * 64).map(|i| (i * 13 % 64 * 1000) as u16).collect();
        let e = Exported { w: 64, h: 64, rgb, gains, headroom: 2.0 };
        for f in PhotoFormat::ALL.into_iter().filter(|f| f.lossy()) {
            assert!(encoded_size(&e, f, 50) < encoded_size(&e, f, 95), "{f:?}");
        }
        // The gain map counts.
        assert!(encoded_size(&e, PhotoFormat::UltraHdr, 90) > encoded_size(&e, PhotoFormat::Jpeg, 90) + 500);
    }

    /// A 64×32 HDR export: a grey ramp, the gain rising to the full headroom on the right.
    fn hdr_sample() -> Exported {
        let (w, h) = (64u32, 32u32);
        let rgb = (0..w * h * 3).map(|i| ((i / 3 % w) * 1000) as u16).collect();
        let gains = (0..w * h).map(|i| (i % w * 65535 / (w - 1)) as u16).collect();
        Exported { w, h, rgb, gains, headroom: 2.0 }
    }

    #[test]
    fn png_pq_is_tagged_rec2100() {
        let mut out = Cursor::new(Vec::new());
        encode(&mut out, &hdr_sample(), PhotoFormat::PngHdr, 90, &sample_tags(64, 32)).unwrap();
        let bytes = out.into_inner();
        let mut reader = png::Decoder::new(Cursor::new(&bytes)).read_info().unwrap();
        let info = reader.info();
        let cicp = info.coding_independent_code_points.expect("cICP");
        assert_eq!((cicp.color_primaries, cicp.transfer_function, cicp.matrix_coefficients), (9, 16, 0));
        assert!(info.icc_profile.is_none() && info.exif_metadata.is_some());
        let peak = info.content_light_level.expect("cLLI").max_content_light_level as f32 / 10_000.0;
        assert!((peak - 4.0 * hdr::SDR_WHITE * hdr_peak_fraction()).abs() < 2.0, "{peak}");
        let mut px = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut px).unwrap();
        assert!(contains(&bytes, b"Alba"));
    }

    /// The brightest pixel of `hdr_sample` (last column, SDR 63000/65535) in display-linear terms.
    fn hdr_peak_fraction() -> f32 {
        crate::photo::srgb_invert_gamma(63000.0 / 65535.0)
    }

    #[test]
    fn jxl_is_rec2100_pq() {
        let bytes = hdr::jxl(&hdr_sample(), 90, &sample_tags(64, 32)).unwrap();
        let dec = jpegxl_rs::decoder_builder().build().unwrap();
        let (meta, px) = dec.decode_with::<u16>(&bytes).unwrap();
        assert_eq!((meta.width, meta.height, px.len()), (64, 32, 64 * 32 * 3));
        // Decoded in the signalled space: PQ, the ramp's top-right pixel near its encoded value.
        let want = hdr::pq_rec2100(&hdr_sample()).samples;
        let at = (31 * 64 + 63) * 3 + 1;
        assert!((px[at] as i32 - want[at] as i32).abs() < 1500, "{} vs {}", px[at], want[at]);
        // libjxl's intensity target for a PQ signal: its 10000 cd/m² scale.
        assert_eq!(meta.intensity_target, 10_000.0);
        assert!(contains(&bytes, b"Exif") && contains(&bytes, b"Alba"));
    }

    #[test]
    fn avif_is_rec2100_pq() {
        let bytes = hdr::avif(&hdr_sample(), 90, &sample_tags(64, 32)).unwrap();
        assert!(contains(&bytes, b"ftypavif"));
        // colr nclx: primaries 9, transfer 16, matrix 9, full range.
        assert!(contains(&bytes, &[b'n', b'c', b'l', b'x', 0, 9, 0, 16, 0, 9, 0x80]));
        assert!(contains(&bytes, b"clli") && contains(&bytes, b"Exif") && contains(&bytes, b"Alba"));
    }
}
