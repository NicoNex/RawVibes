//! The HDR rendition of an export as Rec.2100 PQ, and its JPEG XL and AVIF encodings.
//!
//! The rendition is the graded SDR (sRGB, Rec.709) in linear light times each
//! pixel's gain (`grade.wgsl` `hdr_gain`), in Rec.2020 with SDR white at the
//! BT.2408 reference of 203 cd/m²: how HDR stills place diffuse white.

use rayon::prelude::*;

use super::Exported;
use crate::meta::Tags;
use crate::photo::{multiply_row1, srgb_invert_gamma};
use crate::profiles::rec709_to_rec2020;

/// SDR white in cd/m² (ITU-R BT.2408 HDR reference white).
pub const SDR_WHITE: f32 = 203.0;

/// Content light level (CTA-861.3), cd/m²: the brightest pixel's max channel and the image average of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub max_cll: f32,
    pub max_fall: f32,
}

/// The rendition as 16-bit Rec.2100 PQ RGB, and its light level.
pub struct Pq {
    pub samples: Vec<u16>,
    pub light: Light,
}

/// SMPTE ST 2084 inverse EOTF: cd/m² to the PQ signal in 0..1.
pub fn pq_encode(nits: f32) -> f32 {
    ultrahdr_rs::color::transfer::pq_oetf((nits / 10_000.0).clamp(0.0, 1.0))
}

/// `img`'s HDR rendition in Rec.2100 PQ: the graded SDR in linear light times each
/// pixel's gain, in Rec.2020 cd/m². One parallel pass; the sRGB decode and the gain
/// come from 65536-entry tables (both inputs are 16-bit).
pub fn pq_rec2100(img: &Exported) -> Pq {
    let m = rec709_to_rec2020();
    let n = img.gains.len();
    let linear: Vec<f32> = (0..=u16::MAX).map(|v| srgb_invert_gamma(v as f32 / 65535.0)).collect();
    let gain: Vec<f32> = (0..=u16::MAX).map(|g| (g as f32 / 65535.0 * img.headroom).exp2() * SDR_WHITE).collect();
    let mut samples = vec![0u16; 3 * n];
    let (max_cll, sum) = samples
        .par_chunks_mut(3)
        .zip(img.rgb.par_chunks(3))
        .zip(img.gains.par_iter())
        .map(|((out, sdr), &g)| {
            let c =
                multiply_row1(&m, &[0, 1, 2].map(|k| linear[sdr[k] as usize])).map(|v| v.max(0.0) * gain[g as usize]);
            for (o, v) in out.iter_mut().zip(c) {
                *o = (pq_encode(v) * 65535.0).round() as u16;
            }
            let peak = c[0].max(c[1]).max(c[2]);
            (peak, peak as f64)
        })
        .reduce(|| (0.0, 0.0), |a, b| (a.0.max(b.0), a.1 + b.1));
    Pq { samples, light: Light { max_cll, max_fall: (sum / n.max(1) as f64) as f32 } }
}

/// JPEG XL (libjxl, the reference encoder) of the PQ rendition: lossy XYB at the
/// JPEG-style `quality`, the Rec.2100 PQ colour encoding signalled, EXIF and XMP boxes.
pub fn jxl(img: &Exported, quality: u8, tags: &Tags) -> Result<Vec<u8>, String> {
    use jpegxl_rs::encode::{ColorEncoding, Metadata};
    use jpegxl_rs::parallel::threads_runner::ThreadsRunner;
    use jpegxl_sys::color::color_encoding::{
        JxlColorEncoding, JxlColorSpace, JxlPrimaries, JxlRenderingIntent, JxlTransferFunction, JxlWhitePoint,
    };

    let pq = pq_rec2100(img);
    let rec2100 = JxlColorEncoding {
        color_space: JxlColorSpace::Rgb,
        white_point: JxlWhitePoint::D65,
        white_point_xy: [0.0; 2],
        primaries: JxlPrimaries::Rec2100,
        primaries_red_xy: [0.0; 2],
        primaries_green_xy: [0.0; 2],
        primaries_blue_xy: [0.0; 2],
        transfer_function: JxlTransferFunction::PQ,
        gamma: 0.0,
        rendering_intent: JxlRenderingIntent::Relative,
    };
    let runner = ThreadsRunner::default();
    let err = |e: jpegxl_rs::EncodeError| format!("JPEG XL: {e}");
    let mut enc = jpegxl_rs::encoder_builder()
        .parallel_runner(&runner)
        .jpeg_quality(quality as f32)
        .use_container(true)
        .color_encoding(ColorEncoding::Custom(rec2100))
        .build()
        .map_err(err)?;
    if !tags.exif.is_empty() {
        // The Exif box starts with the offset of the TIFF header: here, right after it.
        enc.add_metadata(&Metadata::Exif(&[&[0u8; 4][..], &tags.exif].concat()), false);
    }
    if let Some(x) = &tags.xmp {
        // Uncompressed: not every reader unpacks Brotli boxes.
        enc.add_metadata(&Metadata::Xmp(x.as_bytes()), false);
    }
    enc.encode::<u16>(&pq.samples, img.w, img.h).map_err(err)
}

/// AVIF (libavif with libaom, the reference AV1 encoder) of the PQ rendition: 10-bit full-range 4:4:4,
/// CICP 9/16/9 (Rec.2100 PQ), its content light level, EXIF and XMP.
pub fn avif(img: &Exported, quality: u8, tags: &Tags) -> Result<Vec<u8>, String> {
    use libavif_sys as av;

    let pq = pq_rec2100(img);
    let clli = |nits: f32| nits.round().clamp(0.0, u16::MAX as f32) as u16;
    // SAFETY: every libavif object is created and destroyed in this block, never
    // shared; `rgb.pixels` points into `pq.samples`, alive and unmodified until
    // `avifImageRGBToYUV` has copied it; the output is copied before it is freed.
    unsafe {
        let image = av::avifImageCreate(img.w, img.h, 10, av::AVIF_PIXEL_FORMAT_YUV444);
        if image.is_null() {
            return Err("AVIF: out of memory".into());
        }
        let encoder = av::avifEncoderCreate();
        let mut out = av::avifRWData::default();
        let result = (|| {
            if encoder.is_null() {
                return Err("AVIF: out of memory".to_string());
            }
            let check = |r: av::avifResult, what: &str| {
                if r == av::AVIF_RESULT_OK { Ok(()) } else { Err(format!("AVIF: {what} failed ({r})")) }
            };
            (*image).yuvRange = av::AVIF_RANGE_FULL;
            (*image).colorPrimaries = av::AVIF_COLOR_PRIMARIES_BT2020 as _;
            (*image).transferCharacteristics = av::AVIF_TRANSFER_CHARACTERISTICS_PQ as _;
            (*image).matrixCoefficients = av::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL as _;
            (*image).clli = av::avifContentLightLevelInformationBox {
                maxCLL: clli(pq.light.max_cll),
                maxPALL: clli(pq.light.max_fall),
            };
            let mut rgb = av::avifRGBImage::default();
            av::avifRGBImageSetDefaults(&mut rgb, image);
            (rgb.format, rgb.depth) = (av::AVIF_RGB_FORMAT_RGB, 16);
            (rgb.pixels, rgb.rowBytes) = (pq.samples.as_ptr() as *mut u8, img.w * 6);
            check(av::avifImageRGBToYUV(image, &rgb), "colour conversion")?;
            if !tags.exif.is_empty() {
                check(av::avifImageSetMetadataExif(image, tags.exif.as_ptr(), tags.exif.len()), "EXIF")?;
            }
            if let Some(x) = &tags.xmp {
                check(av::avifImageSetMetadataXMP(image, x.as_ptr(), x.len()), "XMP")?;
            }
            (*encoder).quality = quality as _;
            (*encoder).speed = 6;
            // Tiles as the image size allows, so every core encodes.
            (*encoder).autoTiling = av::AVIF_TRUE as _;
            (*encoder).maxThreads = std::thread::available_parallelism().map_or(1, |n| n.get()) as _;
            check(av::avifEncoderWrite(encoder, image, &mut out), "encoding")?;
            Ok(std::slice::from_raw_parts(out.data, out.size).to_vec())
        })();
        av::avifRWDataFree(&mut out);
        if !encoder.is_null() {
            av::avifEncoderDestroy(encoder);
        }
        av::avifImageDestroy(image);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_hits_its_reference_points() {
        // ST 2084 / BT.2100 table values: 100 cd/m² ≈ 0.508, 203 ≈ 0.581, 1000 ≈ 0.752, 10000 = 1.
        for (nits, want) in [(0.0, 0.0), (100.0, 0.5081), (203.0, 0.5807), (1000.0, 0.7518), (10_000.0, 1.0)] {
            assert!((pq_encode(nits) - want).abs() < 5e-4, "{nits}: {}", pq_encode(nits));
        }
    }

    #[test]
    fn rendition_is_sdr_times_the_gain() {
        // White with no gain is SDR white; with the full gain, `headroom` stops above it.
        let img = Exported { w: 2, h: 1, rgb: vec![65535; 6], gains: vec![0, 65535], headroom: 2.0 };
        let pq = pq_rec2100(&img);
        let back = |v: u16| v as f32 / 65535.0;
        assert!((back(pq.samples[0]) - pq_encode(SDR_WHITE)).abs() < 1e-4);
        assert!((back(pq.samples[3]) - pq_encode(4.0 * SDR_WHITE)).abs() < 1e-4);
        assert!((pq.light.max_cll - 4.0 * SDR_WHITE).abs() < 0.5);
        assert!((pq.light.max_fall - 2.5 * SDR_WHITE).abs() < 0.5);
    }
}
