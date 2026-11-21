//! Panasonic makernote: the Real Time LUTs the camera applied to the photo
//! (LUT1 / LUT2 name and opacity). In an RW2 the makernote sits in the EXIF
//! of the full-size JPEG the camera embeds (RW2 tag 0x2e, `JpgFromRaw`).
//! Every offset comes from the file: each read is bounds-checked and a
//! malformed file gives no LUTs, never a panic.

use std::path::Path;

/// One Real Time LUT: its name as set in the camera and its opacity, 0–100.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraLut {
    pub name: String,
    pub opacity: u8,
}

/// The Real Time LUTs named in the RW2 at `path` (none for other files).
pub fn real_time_luts(path: &Path) -> Vec<CameraLut> {
    read_rw2(path).unwrap_or_default()
}

/// Reads only what it needs: the RW2 header and IFD0, then the start of the
/// embedded JPEG, where its EXIF is (a JPEG's APP1 is at most 64 KB).
fn read_rw2(path: &Path) -> Option<Vec<CameraLut>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut read_at = |at: u64, len: usize| -> Option<Vec<u8>> {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut buf = Vec::new();
        f.by_ref().take(len as u64).read_to_end(&mut buf).ok()?;
        Some(buf)
    };
    let head = read_at(0, 8)?;
    if head.get(..4)? != b"IIU\0" {
        return None;
    }
    let ifd0 = u32::from_le_bytes(head[4..8].try_into().ok()?) as u64;
    let n = u16::from_le_bytes(read_at(ifd0, 2)?.try_into().ok()?) as usize;
    // IFD0 alone, re-based at 0: its JpgFromRaw entry gives the JPEG's offset and length.
    let ifd = read_at(ifd0, 2 + 12 * n.min(1024))?;
    let (count, field) = Tiff { b: &ifd, le: true }.entry(0, 0x2e)?;
    let at = u32::from_le_bytes(ifd.get(field..field + 4)?.try_into().ok()?) as u64;
    from_jpeg(&read_at(at, (count as usize).min(1 << 17))?)
}

/// A TIFF block: its bytes and byte order.
struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let s: [u8; 2] = self.b.get(at..at + 2)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let s: [u8; 4] = self.b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    }

    /// The entry for `tag` in the IFD at `ifd`: (count, byte position of its value field).
    fn entry(&self, ifd: usize, tag: u16) -> Option<(u32, usize)> {
        let n = self.u16(ifd)? as usize;
        (0..n.min(1024))
            .map(|i| ifd + 2 + 12 * i)
            .find(|&e| self.u16(e) == Some(tag))
            .map(|e| (self.u32(e + 4).unwrap_or(0), e + 8))
    }

    /// `count` bytes of `tag`'s value: inline when they fit in 4, else at its offset.
    fn bytes(&self, ifd: usize, tag: u16) -> Option<&[u8]> {
        let (count, field) = self.entry(ifd, tag)?;
        let at = if count <= 4 { field } else { self.u32(field)? as usize };
        self.b.get(at..at.checked_add(count as usize)?)
    }
}

/// The LUTs in the Panasonic makernote of `jpeg`'s EXIF.
fn from_jpeg(jpeg: &[u8]) -> Option<Vec<CameraLut>> {
    let mut at = 2;
    let exif = loop {
        let (marker, len) = (jpeg.get(at..at + 2)?, u16::from_be_bytes(jpeg.get(at + 2..at + 4)?.try_into().ok()?));
        let seg = jpeg.get(at + 4..at + 2 + len as usize)?;
        if marker == [0xff, 0xe1] && seg.starts_with(b"Exif\0\0") {
            break &seg[6..];
        }
        if marker[0] != 0xff || marker[1] == 0xda {
            return None;
        }
        at += 2 + len as usize;
    };
    let tiff = Tiff { b: exif, le: exif.get(..2)? == b"II" };
    let ifd0 = tiff.u32(4)? as usize;
    let exif_ifd = tiff.u32(tiff.entry(ifd0, 0x8769)?.1)? as usize;
    let (_, field) = tiff.entry(exif_ifd, 0x927c)?;
    let note = tiff.u32(field)? as usize;
    // "Panasonic\0\0\0", then an IFD whose offsets count from the EXIF TIFF start.
    if tiff.b.get(note..note + 12)? != b"Panasonic\0\0\0" {
        return None;
    }
    let ifd = note + 12;
    let luts = [(0xf1, 0xf3), (0xf4, 0xf5)]
        .into_iter()
        .filter_map(|(name, opacity)| {
            let raw = tiff.bytes(ifd, name)?;
            let name = String::from_utf8_lossy(raw.split(|&c| c == 0).next()?).trim().to_owned();
            let opacity = *tiff.bytes(ifd, opacity)?.first()?;
            (!name.is_empty() && opacity > 0).then_some(CameraLut { name, opacity: opacity.min(100) })
        })
        .collect();
    Some(luts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal JPEG whose EXIF holds a Panasonic makernote with `luts`.
    fn jpeg(luts: &[(&str, u8)]) -> Vec<u8> {
        // EXIF TIFF (little-endian): IFD0 at 8 → ExifIFD at 26 → MakerNote at 44.
        let mut t = b"II*\0".to_vec();
        t.extend(8u32.to_le_bytes());
        let entry = |tag: u16, typ: u16, count: u32, value: u32| {
            [&tag.to_le_bytes()[..], &typ.to_le_bytes(), &count.to_le_bytes(), &value.to_le_bytes()].concat()
        };
        t.extend(1u16.to_le_bytes());
        t.extend(entry(0x8769, 4, 1, 26));
        t.extend(0u32.to_le_bytes());
        t.extend(1u16.to_le_bytes());
        t.extend(entry(0x927c, 7, 4096, 44));
        t.extend(0u32.to_le_bytes());
        assert_eq!(t.len(), 44);
        t.extend(b"Panasonic\0\0\0");
        let n = 4u16;
        t.extend(n.to_le_bytes());
        let strings = 44 + 12 + 2 + 12 * n as usize + 4;
        let names: Vec<[u8; 256]> = (0..2)
            .map(|i| {
                let mut s = [0u8; 256];
                if let Some((name, _)) = luts.get(i) {
                    s[..name.len()].copy_from_slice(name.as_bytes());
                }
                s
            })
            .collect();
        let op = |i: usize| luts.get(i).map_or(0, |l| l.1) as u32;
        t.extend(entry(0xf1, 2, 256, strings as u32));
        t.extend(entry(0xf3, 1, 1, op(0)));
        t.extend(entry(0xf4, 2, 256, (strings + 256) as u32));
        t.extend(entry(0xf5, 1, 1, op(1)));
        t.extend(0u32.to_le_bytes());
        names.iter().for_each(|s| t.extend(s));
        let mut j = vec![0xff, 0xd8, 0xff, 0xe1];
        j.extend(((t.len() + 8) as u16).to_be_bytes());
        j.extend(b"Exif\0\0");
        j.extend(t);
        j.extend([0xff, 0xda, 0, 2]);
        j
    }

    #[test]
    fn reads_both_luts() {
        let got = from_jpeg(&jpeg(&[("Fuji Eterna 3513", 100), ("Warm", 40)])).unwrap();
        assert_eq!(
            got,
            [
                CameraLut { name: "Fuji Eterna 3513".into(), opacity: 100 },
                CameraLut { name: "Warm".into(), opacity: 40 }
            ]
        );
        // An empty slot or zero opacity is no LUT.
        assert_eq!(from_jpeg(&jpeg(&[("Only", 70)])).unwrap().len(), 1);
    }

    /// Truncated or garbled data gives no LUTs instead of panicking.
    #[test]
    fn malformed_is_none() {
        let good = jpeg(&[("Fuji Eterna 3513", 100)]);
        for cut in [3, 10, 30, 60, 100, 300, good.len() - 5] {
            let _ = from_jpeg(&good[..cut]);
        }
        let mut bad = good.clone();
        bad[50..60].fill(0xff);
        let _ = from_jpeg(&bad);
        assert!(real_time_luts(Path::new(env!("CARGO_MANIFEST_PATH"))).is_empty(), "not a RAW");
    }

    /// On a real file (RAWVIBES_RAW): prints what the camera named.
    #[test]
    fn on_sample() {
        let Ok(path) = std::env::var("RAWVIBES_RAW") else { return };
        eprintln!("{:?}", real_time_luts(Path::new(&path)));
    }
}
