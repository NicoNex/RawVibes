//! Photo metadata: read from the source (RAW, JPEG, TIFF), completed with the
//! author's defaults, filtered by the export policy, written as EXIF and XMP.

use std::io::Cursor;
use std::path::Path;

pub use rawler::decoders::RawMetadata;
pub use rawler::exif::Exif;
pub use rawler::formats::tiff::{Rational, SRational};
use rawler::formats::jfif::Jfif;
use rawler::formats::tiff::reader::TiffReader;
use rawler::formats::tiff::{DirectoryWriter, GenericTiffReader, IFD, TiffWriter};
use rawler::tags::{ExifTag, TiffCommonTag};

use crate::sidecar::Meta;

/// The camera's metadata of `path`; `None` when it has none or cannot be read.
pub fn read(path: &Path) -> Option<RawMetadata> {
    if crate::photo::is_raw(path) {
        let src = rawler::rawsource::RawSource::new(path).ok()?;
        return rawler::get_decoder(&src).ok()?.raw_metadata(&src, &Default::default()).ok();
    }
    let bytes = std::fs::read(path).ok()?;
    let (exif, make, model) = match bytes.get(..4)? {
        [0xff, 0xd8, ..] => {
            let jfif = Jfif::parse(Cursor::new(&bytes)).ok()?;
            let root = jfif.exif_ifd()?;
            (Exif::new(root).ok()?, ascii(root, TiffCommonTag::Make), ascii(root, TiffCommonTag::Model))
        }
        b"II*\0" | b"MM\0*" => read_tiff_exif(&bytes)?,
        _ => return None,
    };
    Some(RawMetadata { exif, make, model, lens: None, unique_image_id: None, rating: None })
}

/// EXIF, make and model of a TIFF stream (a TIFF file, or a JPEG's EXIF block).
pub fn read_tiff_exif(tiff: &[u8]) -> Option<(Exif, String, String)> {
    let subs = [TiffCommonTag::ExifIFDPointer.into(), ExifTag::GPSInfo.into()];
    let reader = GenericTiffReader::new(&mut Cursor::new(tiff), 0, 0, Some(0), &subs).ok()?;
    let root = reader.file().chain.first()?;
    Some((Exif::new(root).ok()?, ascii(root, TiffCommonTag::Make), ascii(root, TiffCommonTag::Model)))
}

fn ascii(ifd: &IFD, tag: TiffCommonTag) -> String {
    ifd.get_entry(tag).and_then(|e| e.value.as_string()).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// `m` with every empty field taken from `d`; `{year}` and `{creator}` expanded in the copyright.
pub fn with_defaults(m: &Meta, d: &Meta, year: Option<i32>) -> Meta {
    let pick = |a: &String, b: &String| if a.trim().is_empty() { b.clone() } else { a.clone() };
    let creator = pick(&m.creator, &d.creator);
    let year = year.unwrap_or_else(current_year).to_string();
    let copyright = pick(&m.copyright, &d.copyright).replace("{year}", &year).replace("{creator}", &creator);
    Meta {
        title: pick(&m.title, &d.title),
        caption: pick(&m.caption, &d.caption),
        keywords: if m.keywords.is_empty() { d.keywords.clone() } else { m.keywords.clone() },
        creator,
        copyright,
        email: pick(&m.email, &d.email),
        website: pick(&m.website, &d.website),
        city: pick(&m.city, &d.city),
        country: pick(&m.country, &d.country),
    }
}

/// This year (UTC), from the system clock.
fn current_year() -> i32 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // Howard Hinnant's civil_from_days, year only.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    (yoe + era * 400 + i64::from(mp >= 10)) as i32
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The XMP packet of `m`; `None` when every field is empty.
pub fn xmp(m: &Meta) -> Option<String> {
    let mut body = String::new();
    let mut put = |tag: &str, inner: String| body.push_str(&format!("<{tag}>{inner}</{tag}>\n"));
    let alt = |v: &str| format!("<rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt>", escape(v));
    let list = |kind: &str, vs: &[String]| {
        let items: String = vs.iter().map(|v| format!("<rdf:li>{}</rdf:li>", escape(v))).collect();
        format!("<rdf:{kind}>{items}</rdf:{kind}>")
    };
    for (tag, v) in [("dc:title", &m.title), ("dc:description", &m.caption), ("dc:rights", &m.copyright)] {
        if !v.trim().is_empty() {
            put(tag, alt(v));
        }
    }
    if !m.creator.trim().is_empty() {
        put("dc:creator", list("Seq", std::slice::from_ref(&m.creator)));
    }
    if !m.keywords.is_empty() {
        put("dc:subject", list("Bag", &m.keywords));
    }
    for (tag, v) in [("photoshop:City", &m.city), ("photoshop:Country", &m.country)] {
        if !v.trim().is_empty() {
            put(tag, escape(v));
        }
    }
    let contact: String = [("CiEmailWork", &m.email), ("CiUrlWork", &m.website)]
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(t, v)| format!("<Iptc4xmpCore:{t}>{}</Iptc4xmpCore:{t}>", escape(v)))
        .collect();
    if !contact.is_empty() {
        body.push_str(&format!(
            "<Iptc4xmpCore:CreatorContactInfo rdf:parseType=\"Resource\">{contact}</Iptc4xmpCore:CreatorContactInfo>\n"
        ));
    }
    if body.is_empty() {
        return None;
    }
    Some(format!(
        concat!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
            "<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
            "xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\" ",
            "xmlns:Iptc4xmpCore=\"http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/\" xmp:CreatorTool=\"RawVibes\">\n",
            "{}</rdf:Description></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>"
        ),
        body
    ))
}

/// The year the photo was taken, from its `DateTimeOriginal` (`YYYY:MM:DD hh:mm:ss`).
pub fn capture_year(src: &RawMetadata) -> Option<i32> {
    src.exif.date_time_original.as_deref()?.get(..4)?.parse().ok()
}

/// How much metadata an exported photo carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    /// Everything, location and serial numbers included.
    All,
    /// Camera, exposure, date, rights and description; no location, no serial numbers.
    #[default]
    Essential,
    /// Creator, copyright and contact only.
    Copyright,
    None,
}

impl Policy {
    pub const ALL: [Policy; 4] = [Policy::All, Policy::Essential, Policy::Copyright, Policy::None];

    pub fn tag(self) -> &'static str {
        match self {
            Policy::All => "all",
            Policy::Essential => "essential",
            Policy::Copyright => "copyright",
            Policy::None => "none",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.tag() == tag)
    }
}

/// What an export embeds: an EXIF block (a TIFF stream, without the `Exif\0\0` header) and an XMP packet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tags {
    pub exif: Vec<u8>,
    pub xmp: Option<String>,
}

/// The tags of an export `size` pixels big: `src`'s camera metadata and `m` (already
/// completed with the defaults), filtered by `policy`.
pub fn tags(src: Option<&RawMetadata>, m: &Meta, policy: Policy, size: (u32, u32)) -> Tags {
    if policy == Policy::None {
        return Tags::default();
    }
    let mut md = src.cloned().unwrap_or_else(|| RawMetadata {
        exif: Exif::default(),
        make: String::new(),
        model: String::new(),
        lens: None,
        unique_image_id: None,
        rating: None,
    });
    let e = &mut md.exif;
    match policy {
        Policy::Essential => {
            (e.gps, e.serial_number, e.lens_serial_number) = (None, None, None);
            (e.owner_name, e.user_comment) = (None, None);
        }
        Policy::Copyright => {
            let (artist, copyright) = (e.artist.take(), e.copyright.take());
            *e = Exif { artist, copyright, ..Default::default() };
            (md.make, md.model) = (String::new(), String::new());
        }
        Policy::All | Policy::None => {}
    }
    // The exported pixels are already upright.
    e.orientation = Some(1);
    // EXIF ASCII fields carry the text as UTF-8 bytes; XMP is the authoritative copy.
    for (field, v) in [(&mut e.artist, &m.creator), (&mut e.copyright, &m.copyright)] {
        if !v.trim().is_empty() {
            *field = Some(v.clone());
        }
    }
    let xmp = if policy == Policy::Copyright {
        let (creator, copyright, email, website) = (m.creator.clone(), m.copyright.clone(), m.email.clone(), m.website.clone());
        self::xmp(&Meta { creator, copyright, email, website, ..Default::default() })
    } else {
        self::xmp(m)
    };
    let title = if policy == Policy::Copyright { "" } else { m.title.trim() };
    // A write failure only loses the EXIF block, never the export.
    let exif = write_exif(&md, title, (policy != Policy::Copyright).then_some(size)).unwrap_or_default();
    Tags { exif, xmp }
}

/// `md` as a TIFF stream: IFD0, the Exif IFD (with `size` and sRGB when given) and the GPS IFD.
fn write_exif(md: &RawMetadata, title: &str, size: Option<(u32, u32)>) -> rawler::formats::tiff::Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    let mut tiff = TiffWriter::new(&mut buf)?;
    let (mut root, mut ex) = (DirectoryWriter::new(), DirectoryWriter::new());
    md.write_exif_tags(&mut tiff, &mut root, &mut ex)?;
    for (tag, v) in [(TiffCommonTag::Make, &md.make), (TiffCommonTag::Model, &md.model)] {
        if !v.is_empty() {
            root.add_tag(tag, v.as_str());
        }
    }
    root.add_tag(TiffCommonTag::Software, concat!("RawVibes ", env!("CARGO_PKG_VERSION")));
    if !title.is_empty() {
        root.add_tag(ExifTag::ImageDescription, title);
    }
    if let Some((w, h)) = size {
        // Required by the EXIF spec for JPEG: centred chroma, Y Cb Cr order.
        root.add_tag(ExifTag::YCbCrPositioning, 1u16);
        ex.add_tag_undefined(ExifTag::ComponentsConfiguration, vec![1, 2, 3, 0]);
        ex.add_tag_undefined(ExifTag::ExifVersion, b"0232".to_vec());
        ex.add_tag(ExifTag::ExifImageWidth, w);
        ex.add_tag(ExifTag::ExifImageHeight, h);
        ex.add_tag(ExifTag::ColorSpace, 1u16);
    }
    if !ex.is_empty() {
        let offset = ex.build(&mut tiff)?;
        root.add_tag(TiffCommonTag::ExifIFDPointer, offset);
    }
    tiff.build(root)?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn fixture() -> RawMetadata {
        let gps = rawler::exif::ExifGPS {
            gps_latitude_ref: Some("N".into()),
            gps_latitude: Some([Rational::new(45, 1), Rational::new(27, 1), Rational::new(0, 1)]),
            ..Default::default()
        };
        let exif = Exif {
            date_time_original: Some("2023:07:14 18:02:11".into()),
            iso_speed_ratings: Some(400),
            serial_number: Some("1234".into()),
            orientation: Some(6),
            gps: Some(gps),
            ..Default::default()
        };
        RawMetadata {
            exif,
            make: "Fujifilm".into(),
            model: "X-T5".into(),
            lens: None,
            unique_image_id: None,
            rating: None,
        }
    }

    #[test]
    fn defaults_fill_only_empty_fields() {
        let m = Meta { creator: "Ada".into(), ..Default::default() };
        let d = Meta { creator: "Bob".into(), copyright: "© {year} {creator}".into(), email: "b@x.it".into(), ..Default::default() };
        let out = with_defaults(&m, &d, Some(2024));
        assert_eq!((out.creator.as_str(), out.copyright.as_str(), out.email.as_str()), ("Ada", "© 2024 Ada", "b@x.it"));
    }

    #[test]
    fn xmp_escapes_markup() {
        let x = xmp(&Meta { title: r#"a<b & "c""#.into(), ..Default::default() }).unwrap();
        assert!(x.contains("a&lt;b &amp; &quot;c&quot;"), "{x}");
        assert!(!x.contains("a<b"));
    }

    #[test]
    fn utf8_survives_in_xmp() {
        let x = xmp(&Meta { creator: "Nicolò ©".into(), keywords: vec!["mare".into(), "luce".into()], ..Default::default() })
            .unwrap();
        assert!(x.contains("Nicolò ©") && x.contains("<rdf:li>mare</rdf:li>") && x.contains("RawVibes"), "{x}");
    }

    #[test]
    fn empty_meta_has_no_xmp() {
        assert_eq!(xmp(&Meta::default()), None);
    }

    #[test]
    fn capture_year_parses() {
        assert_eq!(capture_year(&fixture()), Some(2023));
        let mut no_date = fixture();
        no_date.exif.date_time_original = None;
        assert_eq!(capture_year(&no_date), None);
    }

    #[test]
    fn unreadable_file_has_no_metadata() {
        assert!(read(Path::new("/nonexistent/P1.RW2")).is_none());
        assert!(read_tiff_exif(b"not a tiff").is_none());
    }

    fn written(policy: Policy) -> (Tags, Option<(Exif, String, String)>) {
        let m = Meta { creator: "Ada".into(), copyright: "© Ada".into(), title: "Alba".into(), ..Default::default() };
        let t = tags(Some(&fixture()), &m, policy, (6000, 4000));
        let back = read_tiff_exif(&t.exif);
        (t, back)
    }

    #[test]
    fn all_round_trips() {
        let (_, back) = written(Policy::All);
        let (exif, make, model) = back.expect("readable EXIF");
        assert_eq!((make.as_str(), model.as_str()), ("Fujifilm", "X-T5"));
        assert_eq!((exif.iso_speed_ratings, exif.orientation), (Some(400), Some(1)));
        assert_eq!(exif.artist.as_deref(), Some("Ada"));
        assert!(exif.gps.is_some_and(|g| g.gps_latitude.is_some()));
        assert_eq!(exif.serial_number.as_deref(), Some("1234"));
    }

    #[test]
    fn essential_drops_gps() {
        let (t, back) = written(Policy::Essential);
        let (exif, make, _) = back.expect("readable EXIF");
        assert!(exif.gps.is_none() && exif.serial_number.is_none());
        assert_eq!((make.as_str(), exif.iso_speed_ratings), ("Fujifilm", Some(400)));
        assert!(t.xmp.unwrap().contains("Alba"));
    }

    #[test]
    fn copyright_keeps_only_rights() {
        let (t, back) = written(Policy::Copyright);
        let (exif, make, _) = back.expect("readable EXIF");
        assert_eq!((exif.artist.as_deref(), exif.copyright.as_deref()), (Some("Ada"), Some("© Ada")));
        assert!(make.is_empty() && exif.iso_speed_ratings.is_none() && exif.gps.is_none());
        let x = t.xmp.unwrap();
        assert!(x.contains("Ada") && !x.contains("Alba"));
    }

    #[test]
    fn none_policy_writes_no_exif() {
        let (t, _) = written(Policy::None);
        assert_eq!(t, Tags::default());
    }

    /// What exiftool's validation requires of a JPEG's EXIF.
    #[test]
    fn exif_has_the_required_tags() {
        let t = tags(None, &Meta::default(), Policy::Essential, (10, 10));
        let reader = GenericTiffReader::new(&mut Cursor::new(&t.exif), 0, 0, Some(0), &[TiffCommonTag::ExifIFDPointer.into()]).unwrap();
        let root = &reader.file().chain[0];
        assert!(root.get_entry(ExifTag::YCbCrPositioning).is_some());
        let ex = root.get_sub_ifd(TiffCommonTag::ExifIFDPointer).unwrap();
        assert!(ex.get_entry(ExifTag::ComponentsConfiguration).is_some());
    }

    #[test]
    fn tags_without_source() {
        let t = tags(None, &Meta { creator: "Ada".into(), ..Default::default() }, Policy::Essential, (10, 10));
        let (exif, make, _) = read_tiff_exif(&t.exif).expect("readable EXIF");
        assert_eq!((exif.artist.as_deref(), make.as_str()), (Some("Ada"), ""));
    }

    #[test]
    fn policy_tags_round_trip() {
        for p in Policy::ALL {
            assert_eq!(Policy::from_tag(p.tag()), Some(p));
        }
        assert_eq!(Policy::from_tag("bogus"), None);
    }
}
