# Metadata and export quality — design

## Why

Exported photos carry no metadata today: camera, date, GPS, author all lost. The user wants
professional metadata handling (on par with Lightroom / darktable): edit a photo's metadata in the
editor, have it in the exported file, keep it in the sidecar so reopening the photo restores it,
choose how much metadata an export carries, and stamp author / copyright automatically.
Exports should also gain the quality steps a pro raw converter takes.

## Goals

1. **Metadata section in the editor** (Photo tab, a new "Metadata" section at the bottom):
   - read-only camera facts: camera, lens, exposure (shutter, aperture, ISO, bias), focal length,
     capture date, GPS present yes/no;
   - editable per photo: title, caption, keywords (comma separated), creator, copyright,
     plus location (city, country). Empty creator / copyright fields show the Preferences default
     as placeholder.
   - Saved in the sidecar (`meta` object; absent in older sidecars = empty).
2. **Preferences → Metadata group**:
   - *Export metadata*: **All** / **Essential** (default) / **Copyright only** / **None**.
     - All: everything below plus GPS, camera and lens serial numbers.
     - Essential: camera, lens, exposure, date, creator, rights, title, caption, keywords,
       location text. No GPS, no serial numbers.
     - Copyright only: creator, copyright, contact.
     - None: nothing (ICC profile still embedded: it is colour, not metadata).
   - *Creator*, *Copyright* (default `© {year} {creator}`, `{year}` = capture year),
     *Email*, *Website*: used when the photo's own field is empty.
   - *Export suffix*: default `_edit` (today hard-coded in `edit_name`); empty is not allowed
     (it would overwrite the source's JPEG twin), falls back to `_edit`.
3. **Writing**: EXIF (TIFF IFD0 + Exif IFD + GPS IFD) built by our own small writer, passed to
   `image`'s `set_exif_metadata`; XMP packet (dc:title, dc:description, dc:subject, dc:creator,
   dc:rights, Iptc4xmpCore location / contact, xmp:CreatorTool = RawVibes) inserted as its own
   APP1 segment in JPEG, `iTXt XML:com.adobe.xmp` chunk in PNG. Always written: Software,
   Orientation = 1 (pixels are already upright), ColorSpace = sRGB, pixel dimensions.
4. **Export quality**:
   - dither the 16 → 8 bit JPEG conversion (blue-noise or Bayer-free random triangular dither,
     ±1 LSB) so skies and gradients do not band;
   - embed an sRGB ICC profile (compact CC0 sRGB v2) in JPEG, PNG and TIFF.

## Non-goals

- Writing `.xmp` sidecars next to the RAW, or reading other apps' XMP (chosen: option A).
- Editing camera facts (date shift, lens fix), batch metadata editing, keyword lists / hierarchies.
- Output sharpening (offered, not requested).
- Metadata in 16-bit TIFF beyond the ICC profile (shortcut, see below).

## Anti-goals

- An export that silently leaks GPS when the user picked Essential / Copyright only / None.
- A metadata write failure that loses the export: the image is written regardless; metadata
  errors only drop metadata.
- New dependencies.

## Approaches

1. **Own EXIF writer + hand-written XMP** — *picked*. `image` already embeds EXIF and ICC in
   JPEG/PNG; ~200 lines of writer, round-trip tested. No dependencies, full control.
2. `little_exif` crate for EXIF + hand-written XMP: less code, covers TIFF, but a new dependency
   and its edge cases.
3. External `exiftool`: most complete, needs Perl + exiftool installed. Rejected.

## Decisions

- Option A storage: sidecar + exported file only (user).
- Option 1 writer, quality steps dithering + ICC (user).
- Lab tab icon: a cube symbolic SVG (my call, no objection).
- Paste edits keeps the target's metadata, like its crop and stars (my call: a title is per photo).
- Source metadata: RAWs via `rawler` `raw_metadata().exif`. JPEG/TIFF sources: only the user's
  fields and defaults are written — `shortcut: no EXIF reader for non-RAW sources, add one when
  camera JPEGs are exported often`.
- TIFF: ICC only — `shortcut: no EXIF/XMP in TIFF, add via tiff crate custom tags when asked`.
- Metadata policy and defaults are global settings, not per photo ("per ogni foto" = applied to
  every export).

## Left to the builder

Exact widget layout of the section, tag list order, dither kernel, where the ICC bytes live
(`include_bytes!`), test images.
