# Metadata and Export Quality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Exports carry the photo's metadata (filtered by a policy, stamped with author defaults) plus an sRGB ICC profile and dithered 8-bit JPEG; a Metadata tab edits per-photo fields saved in the sidecar; export suffix configurable; crop editing ends by itself; LUT Lab gets a cube icon.

**Architecture:** New `core/src/meta.rs` reads source metadata (rawler for RAW/JPEG/TIFF), merges user fields and defaults, filters by policy, and builds EXIF bytes (rawler's `TiffWriter` + `RawMetadata::write_exif_tags`) and an XMP packet. `core/src/export.rs` embeds them with ICC and dithering. The GTK side adds a Metadata tab (same pattern as Photo/Grade), a Preferences page, and the sidecar field.

**Tech Stack:** Rust, rawler 0.8, image 0.25 (JPEG/PNG/TIFF encoders), GTK4 + libadwaita via relm4.

**Spec:** `docs/superpowers/specs/2026-11-22-metadata-export-design.md`

## Global Constraints

- No new dependencies (Cargo.toml `[dependencies]` unchanged).
- Every user-visible string through `gettext`, translated in `po/it.po` (`make pot`, then fill the Italian `msgstr`).
- A metadata failure never fails an export: on error write the image without metadata.
- GPS and serial numbers only with policy **All**.
- Default suffix `_edit`; empty or whitespace-only → `_edit`; path separators stripped.
- Default copyright template `© {year} {creator}`; `{year}` = capture year, else the current year.
- Commit after each task, push after each commit (project rule). Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit: `cargo clippy -q --workspace` clean, `cargo test -q --workspace` green.

## Review Focus

1. Photo with no EXIF at all (screenshot PNG/JPEG) → export still works, writes only user fields/defaults; test in Task 2 (`tags_without_source`).
2. Non-ASCII text (`Nicolò`, `©`, emoji) in creator/title → EXIF ASCII fields must not corrupt; XMP carries full UTF-8. Test in Task 2 (`utf8_survives_in_xmp`), EXIF ASCII fields get the non-ASCII chars replaced, not dropped silently with the whole tag.
3. XML special characters (`<`, `&`, `"`) in title/keywords → escaped in XMP. Test in Task 2 (`xmp_escapes_markup`).
4. Policy None / Copyright only must leave no GPS bytes anywhere in the file. Test in Task 3 (`none_policy_writes_no_exif`, `essential_drops_gps`).
5. Suffix `"../x"` or `" "` → safe file name. Test in Task 6 (`suffix_is_sanitized`).

---

### Task 1: Sidecar `meta` field

**Files:**
- Modify: `core/src/sidecar.rs` (struct `Sidecar`, the serde `File` struct and its `From`/`TryFrom` conversions ~lines 140–205, doc comment at top)
- Modify: `gtk/src/main.rs` (`sidecar_state`, `restore_sidecar`, `pasted`, `record_edits`)

**Interfaces:**
- Produces: `rawvibes_core::sidecar::Meta { title: String, caption: String, keywords: Vec<String>, creator: String, copyright: String, email: String, website: String, city: String, country: String }` — `#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]`, `#[serde(default)]` on the struct; `Sidecar.meta: Meta`.

- [ ] **Step 1: Failing test** in `sidecar.rs` tests: `meta_round_trips` — a sample with `meta.title = "Alba"`, `keywords = ["mare", "luce"]` saves and loads equal; `old_sidecar_has_empty_meta` — the existing v2 JSON sample without `"meta"` loads with `Meta::default()`.
- [ ] **Step 2:** `cargo test -q -p rawvibes-core sidecar` → FAIL (no field).
- [ ] **Step 3:** Add `Meta` and the field (no version bump: absent = default).
- [ ] **Step 4:** GTK: `App.meta: Meta`; `sidecar_state` writes it; `restore_sidecar` sets it (UI sync comes in Task 5); `pasted` keeps `target.meta`; `record_edits`' `same` ignores `meta` like rating/label (text edits are not undo steps). Extend `paste_keeps_crop_and_marks` to assert `p.meta == target.meta`.
- [ ] **Step 5:** tests pass; commit "Sidecar keeps the photo's metadata".

### Task 2: `meta.rs` — read, merge, XMP

**Files:**
- Create: `core/src/meta.rs` (+ `pub mod meta;` in `core/src/lib.rs`)

**Interfaces:**
- Consumes: `Meta` (Task 1).
- Produces:
  - `pub fn read(path: &Path) -> Option<rawler::decoders::RawMetadata>` — RAW: `rawler::get_decoder(&src)?.raw_metadata(&src, &Default::default())`; JPEG: `rawler::formats::jfif::Jfif::new(&src)` → `exif_ifd()` → `Exif::new`, make/model from the root IFD; TIFF: `read_tiff_exif(&bytes)`. Build `RawMetadata` by struct literal (fields are pub).
  - `pub fn read_tiff_exif(tiff: &[u8]) -> Option<(Exif, String, String)>` — `GenericTiffReader::new_with_buffer(tiff, 0, 0, None)`, root IFD + Exif sub-IFD + GPS sub-IFD; returns exif, make, model. Also used by tests for round trips.
  - `pub fn with_defaults(m: &Meta, d: &Meta, year: Option<i32>) -> Meta` — every empty field of `m` takes `d`'s; `{year}`/`{creator}` expanded in copyright (year = `year` or current year).
  - `pub fn xmp(m: &Meta) -> Option<String>` — `None` when every field is empty; otherwise an `<x:xmpmeta>` packet with `dc:title`, `dc:description` (rdf:Alt x-default), `dc:subject` (rdf:Bag), `dc:creator` (rdf:Seq), `dc:rights`, `photoshop:City`, `photoshop:Country`, `Iptc4xmpCore:CreatorContactInfo` (`CiEmailWork`, `CiUrlWork`), `xmp:CreatorTool="RawVibes"`. Text XML-escaped.
  - `pub fn capture_year(src: &RawMetadata) -> Option<i32>` — from `date_time_original` (`YYYY:MM:DD …`).

- [ ] **Step 1: Failing tests:**
  - `defaults_fill_only_empty_fields`: `with_defaults(&Meta{creator:"Ada".into(),..}, &Meta{creator:"Bob".into(), copyright:"© {year} {creator}".into(),..}, Some(2024))` → creator `"Ada"`, copyright `"© 2024 Ada"`.
  - `xmp_escapes_markup`: title `a<b & "c"` → packet contains `a&lt;b &amp; &quot;c&quot;`, not `a<b`.
  - `utf8_survives_in_xmp`: creator `Nicolò ©` appears verbatim.
  - `empty_meta_has_no_xmp`: `xmp(&Meta::default()) == None`.
  - `capture_year_parses`: `"2023:07:14 18:02:11"` → `Some(2023)`.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** pass; commit "Metadata: read from RAW/JPEG/TIFF, author defaults, XMP".

### Task 3: `meta.rs` — policy and EXIF writer

**Files:**
- Modify: `core/src/meta.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub enum Policy { All, #[default] Essential, Copyright, None }` with `pub const ALL: [Policy; 4]`, `pub fn tag(self) -> &'static str` (`"all"|"essential"|"copyright"|"none"`), `pub fn from_tag(&str) -> Option<Self>`.
  - `pub struct Tags { pub exif: Vec<u8>, pub xmp: Option<String> }` (`Default` = nothing).
  - `pub fn tags(src: Option<&RawMetadata>, m: &Meta, policy: Policy, size: (u32, u32)) -> Tags` — `m` already merged with defaults. All: source exif as is. Essential: `gps`, `serial_number`, `lens_serial_number`, `owner_name`, `user_comment` cleared. Copyright: only `artist`/`copyright` (+ creator/rights/contact in XMP; title, caption, keywords, location dropped). None: `Tags::default()`. In every non-None case: `orientation = Some(1)`, `artist`/`copyright` replaced by `m.creator`/`m.copyright` when non-empty, root IFD gets Make, Model (not under Copyright), Software `RawVibes <version>`, ImageDescription = title (ASCII-folded); Exif IFD gets PixelXDimension/PixelYDimension = `size`, ColorSpace = 1. EXIF written with `rawler::formats::tiff::writer::TiffWriter::new(Cursor<Vec<u8>>)`, `RawMetadata::write_exif_tags`, Exif IFD built and linked with `TiffCommonTag::ExifIFDPointer`. A writer error → `Tags { exif: vec![], xmp }`.

- [ ] **Step 1: Failing tests** (build a `RawMetadata` fixture with make `"Fujifilm"`, model `"X-T5"`, iso 400, `date_time_original`, a GPS block, `serial_number`):
  - `all_round_trips`: `read_tiff_exif(&tags(Some(&src), &m, Policy::All, (6000, 4000)).exif)` → make/model equal, iso 400, gps `Some`, orientation 1.
  - `essential_drops_gps`: same with Essential → gps `None`, serial `None`, iso 400.
  - `copyright_keeps_only_rights`: artist = `m.creator`, make empty, iso `None`; `xmp` has creator but no title.
  - `none_policy_writes_no_exif`: `tags(..., Policy::None, ..)` → `exif.is_empty() && xmp.is_none()`.
  - `tags_without_source`: `tags(None, &Meta{creator:"Ada".into(),..}, Policy::Essential, (10,10))` → parses, artist `"Ada"`.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement. **Step 4:** pass; commit "Metadata: export policy and EXIF writer".

### Task 4: Export embeds EXIF/XMP/ICC; dithered JPEG

**Files:**
- Create: `core/src/srgb.icc` — `sRGB-v2-micro.icc` from github.com/saucecontrol/Compact-ICC-Profiles (CC0; note source + licence in a one-line comment where it is `include_bytes!`'d).
- Modify: `core/src/export.rs` (`save`, `encode_jpeg`, `to_u8`), `core/src/gpu.rs` (`PhotoExport` gets `pub tags: Tags`; the `Job::ExportPhoto` arm passes `&e.tags`), `gtk/src/main.rs` (`export_to` fills `tags`, see Task 6 for settings).

**Interfaces:**
- Consumes: `Tags` (Task 3).
- Produces: `pub fn save(path, w, h, rgb, format, quality, tags: &Tags, cancelled) -> Result<bool, String>`.
- JPEG: `JpegEncoder` into a `Vec<u8>` with `set_exif_metadata(tags.exif)` (when non-empty) and `set_icc_profile(SRGB_ICC)`, then the XMP APP1 (`http://ns.adobe.com/xap/1.0/\0` + packet; skipped if > 65502 bytes) inserted before the first non-APPn segment by `fn insert_app1(jpeg: Vec<u8>, payload: &[u8]) -> Vec<u8>`. PNG: `PngEncoder` with exif + ICC, XMP as an `iTXt` chunk keyword `XML:com.adobe.xmp` inserted before the first `IDAT` (`fn insert_png_chunk`, own CRC-32, polynomial 0xEDB88320). TIFF: ICC only (`shortcut:` comment per spec).
- Dither: `fn to_u8_dithered(v: u16, i: usize) -> u8` — triangular (TPDF) ±1 LSB noise from a hash of the sample index (no RNG crate), round, clamp 0–255. Replaces `to_u8` in `encode_jpeg`.

- [ ] **Step 1: Failing tests** in `export.rs`:
  - `dither_keeps_the_mean`: for `v` in `[0, 1000, 32768, 65535]`, mean of `to_u8_dithered(v, i)` over `i in 0..100_000` within 0.02 of `v as f64 * 255.0 / 65535.0`; `v = 0` never above 1, `v = 65535` never below 254.
  - `jpeg_carries_metadata`: `save` a 16×8 JPEG with `Tags{exif: <from tags(None, Meta{creator:"Ada"}, Essential,(16,8))>, xmp: Some(..)}`; the bytes contain `b"Exif\0\0"`, `b"ICC_PROFILE"`, `b"http://ns.adobe.com/xap/1.0/"`; `image::load_from_memory` still decodes 16×8.
  - `png_carries_metadata`: same for PNG16: contains `b"eXIf"`, `b"iCCP"`, `b"iTXt"`; decodes.
  - Extend `formats_round_trips` to pass `&Tags::default()`.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement (gtk side passes `Tags::default()` for now so it compiles). **Step 4:** pass; commit "Export: sRGB profile, metadata, dithered JPEG".

### Task 5: Metadata tab

**Files:**
- Create: `gtk/src/meta_ui.rs` (+ `mod meta_ui;`)
- Modify: `gtk/src/main.rs` (tab stack ~line 3173, `section_tabs` ~1810, the visible-child notify, `Msg`, `load_active`, `restore_sidecar`)

**Interfaces:**
- Consumes: `Meta`, `meta::read`, `meta::with_defaults` (placeholders).
- Produces: `pub struct MetaUi { pub root: gtk::Box, ... }`, `MetaUi::new(sender) -> Self`, `fn show_camera(&self, src: Option<&RawMetadata>)`, `fn set(&self, m: &Meta, defaults: &Meta)`; `Msg::MetaEdit(MetaField, String)`, `Msg::SourceMeta(u64 /*load gen*/, Option<Arc<RawMetadata>>)`; `enum MetaField { Title, Caption, Keywords, Creator, Copyright, City, Country }`.

- Tab: `stack.add_titled_with_icon(&meta_tab, Some("meta"), &gettext("Metadata"), "document-properties-symbolic")`, a canvas tab like Photo/Grade (the split moves there); sidebar shows `MetaUi.root` only on `"meta"`. `section_tabs` gains `("meta", "document-properties-symbolic", gettext("Metadata: title, keywords, author (Alt+4)"))`; add the Alt+4 shortcut next to Alt+1..3.
- Layout with the house `section()`: **Camera** (read-only rows: camera, lens, exposure `1/250 s · f/2.8 · ISO 400 · +0.3 EV`, focal length, date, location `Yes/No`), **Description** (title, caption, keywords comma-separated), **Rights** (creator, copyright — placeholder = the Preferences default), **Location** (city, country). Text rows: `adw::EntryRow`.
- `load_active`'s thread also emits `Msg::SourceMeta(generation, meta::read(&path).map(Arc::new))`; stale generations dropped. Edits update `self.meta` and `schedule_save()`; `restore_sidecar` calls `set` (entries echo the same text: no-op).
- [ ] **Step 1:** pure test in main.rs tests: `keywords_split_and_trim` — `parse_keywords(" mare, luce ,,alba ") == ["mare","luce","alba"]` (`fn parse_keywords(&str) -> Vec<String>`), and `exposure_line` formats `1/250 s · f/2.8 · ISO 400 · +0.3 EV` from a fixture `Exif`.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** pass.
- [ ] **Step 5: Manual check:** `make run`, open a RAW: Camera rows filled; type a title, switch photo and back: title restored; Metadata tab keeps the canvas visible. Commit "Metadata tab".

### Task 6: Preferences → Metadata; export wiring; suffix

**Files:**
- Modify: `gtk/src/settings.rs` (keys), `gtk/src/main.rs` (Preferences dialog ~3453–3495, `export_to`, `edit_name` + its 2 callers at ~857 and ~4473)

**Interfaces:**
- Produces: settings keys `METADATA_POLICY = "metadata-policy"`, `EXPORT_SUFFIX = "export-suffix"`, `AUTHOR = "author"`, `COPYRIGHT = "copyright"`, `AUTHOR_EMAIL = "author-email"`, `AUTHOR_WEBSITE = "author-website"`; `fn author_defaults() -> Meta` (copyright defaults to `© {year} {creator}`); `fn edit_name(stem: &str, ext: &str, suffix: &str) -> String`; `fn export_suffix() -> String`.
- New `adw::PreferencesPage` "Metadata" (`document-properties-symbolic`): group **Export** — ComboRow *Metadata in exported photos* (All / Essential / Copyright only / None, subtitle per choice: All "Everything, location and serial numbers included" …), EntryRow *File name suffix*; group **Author** — EntryRows Creator, Copyright (subtitle explains `{year}` and `{creator}`), Email, Website. Each row saves on change.
- `export_to`: `tags = meta::tags(self.source_meta.as_deref(), &meta::with_defaults(&self.meta, &author_defaults(), year), policy, Decoded::fit_size(&crop, u32::MAX))`; policy from settings, default Essential.
- [ ] **Step 1: Failing test** `suffix_is_sanitized`: `edit_name("IMG_1", "jpg", "")` → `"IMG_1_edit.jpg"`, `"  "` → `_edit`, `"../x"` → `"IMG_1..x.jpg"` (separators `/` and `\` removed), `"-web"` → `"IMG_1-web.jpg"`.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** pass.
- [ ] **Step 5: Manual check:** set Creator, export a RAW as JPEG with Essential; `exiftool` (if installed) or `strings out.jpg | grep -a Creator` shows the creator, no GPS; switch to None → no `Exif` in the file. Commit "Preferences: export metadata, author, file suffix".

### Task 7: Crop edit ends by itself

**Files:**
- Modify: `gtk/src/main.rs` (sidebar pane construction, the stack's visible-child notify, `Msg::CropEdit`)

- A `gtk::GestureClick` in `PropagationPhase::Capture` on the sidebar pane: on press, if crop editing is on and the picked widget (`pane.pick(x, y, PickFlags::DEFAULT)`) is not inside the Crop section's root (`is_ancestor`), send `Msg::CropEdit(false)`; the click still reaches its control (do not claim the gesture).
- Tab switch (visible-child notify) sends `Msg::CropEdit(false)`.
- [ ] **Step 1: Manual check** (GTK event routing has no unit-test seam here): `make run`, Edit crop, drag a corner, click the Exposure slider → grid gone, crop kept, slider moved; Edit crop again, switch to Grade → grid gone; the Straighten slider and the level tool keep the grid while used. Commit "Crop editing ends when you move on".

### Task 8: LUT Lab cube icon

**Files:**
- Create: `data/icons/hicolor/scalable/actions/rawvibes-lut-symbolic.svg` (16×16 symbolic: an isometric cube outline, 1 px strokes, `fill:#2e3436`, GNOME symbolic style)
- Modify: `gtk/src/main.rs` (icon name in `section_tabs` and `add_titled_with_icon` for `"lab"`; at startup `gtk::IconTheme::for_display(&display).add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/icons"))` for dev runs), `Makefile` (`install`/`uninstall`: `$(PREFIX)/share/icons/hicolor/scalable/actions/rawvibes-lut-symbolic.svg`)
- [ ] **Step 1: Manual check:** `make run` shows the cube in both tab rows, light and dark theme; `make install DESTDIR=/tmp/x PREFIX=/usr` places the SVG. Commit "LUT Lab: cube icon".

### Task 9: Translations and final pass

- [ ] `make pot`; fill every new `msgstr` in `po/it.po`; `msgfmt --check po/it.po`.
- [ ] `cargo clippy -q --workspace`, `cargo test -q --workspace`, `make arch` green.
- [ ] Review + simplify the branch (project rule for big changes), then commit "Italian strings for metadata" and push.
