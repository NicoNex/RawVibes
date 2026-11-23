# Handoff — HDR (written 2026-11-23)

Goal (user, 2026-11-23): full HDR support built with state-of-the-art
algorithms, formats and libraries. This note records what was researched and
what each part would mean for RawVibes.

## Status

Step 1 is built (2026-11-23): the HDR rendition and the Ultra HDR export.
- Rendition, `grade.wgsl` `hdr_gain`. The view transform's extended Reinhard
  shoulder with its output white raised to the peak P = 2^headroom:
  hdr(m) = m (1 + m P / W²) / (1 + m / P).
  - P = 1 is the SDR curve exactly. Both reach white at the same scene value W.
  - The gain hdr/sdr of the exposed scene's max channel is one factor per
    pixel, so it keeps hue. It is applied over the graded SDR.
  - The scene comes from the log decode (log inputs) or from the developed
    RAW's alpha (`raw.wgsl` `develop_encode` writes it; grade param `scene`).
    A Standard JPEG or TIFF has no scene: gain 1.
  - We chose the Reinhard family over porting ACES 2.0 or sigmoid, because
    the SDR rendition must stay exactly today's.
- Export, `export.rs` `ultra_hdr`.
  - The gain map is half resolution, single channel, gamma 1, offsets 0.
  - Metadata is ISO 21496-1 plus Ultra HDR v1 XMP, built with `ultrahdr-rs`'s
    metadata builders.
  - The container is assembled here, so the photo's XMP (IPTC) stays in one
    packet with the container directory.
  - The UI adds the format "JPEG HDR" and an HDR headroom setting (1–3 stops,
    default 2) in the export dialog.
- Tests:
  - `hdr_gain_extends_only_the_highlights` (GPU): gain against the reference;
    1 at black and at 0 stops; the peak at W; monotone in the scene and the
    headroom; RAW equals V-Log.
  - `ultra_hdr_carries_its_gain_map`: the file read back by ultrahdr-rs's
    decoder.
  - The export strips test now checks the RAW scene plumbing.
- Checked on a real RW2: exiftool reads the MPF, the hdrgm fields and the
  container directory.
- Not done yet:
  - The headroom lives in the export dialog. It moves to the Photo tab with
    the HDR preview (step 2).
  - The decoded file has not been viewed on an HDR screen in Chrome or on
    Android.

HDR covers three separate jobs, plus bracket merge (roadmap phase 9).

## Where we start

- Development is already scene-linear Rec.2020 (`raw.wgsl`, `develop.rs`).
  That is the right base: highlights above 1.0 survive up to the grade.
- Grade and LUTs are SDR by construction: Rec.709 display-referred, or a log
  signal converted to Rec.709. They must stay that way, because camera LUTs
  (.cube, .vlt) are SDR.
- Export is sRGB: JPEG 8-bit, PNG and TIFF 16-bit, with an sRGB ICC
  (`core/src/export.rs`).
- Preview is an RGBA8 sRGB `gdk::MemoryTexture` (`gtk/src/main.rs`,
  `texture()`).
- Monitor colour: on macOS the sRGB preview is converted to the display's
  ColorSync profile with moxcms (`gtk/src/display_color.rs`). On Wayland the
  compositor manages sRGB surfaces itself (GNOME ≥ 48 and KDE 6 expose
  `wp_color_manager_v1`). The user's GNOME 51 does, on an HDR LG ULTRAGEAR
  with HDR switched on.

## 1. HDR preview

- Linux, state of the art: GTK ≥ 4.16 `GdkColorState` (`rec2100-pq`,
  `rec2100-linear`; GTK 4.17+ adds CICP). The app hands GTK float16 textures
  (`R16g16b16a16_float`) tagged with that colour state. GTK passes the image
  description to the compositor, and the compositor maps it to the panel.
- In GTK 4.24 Wayland colour management is still opt-in. Verified on the
  user's build: `GDK_DEBUG=color-mgmt` ("Enable color management") and
  `GDK_DEBUG=hdr` ("Force HDR rendering"). Set it in `main()` before GTK
  initialises; see tsukimi PR #40 for the same move. A third-party report says
  GTK 4.24.1 still needs a separate `color-representation` fix, so check the
  current GTK release notes first.
- macOS: GTK's backend hands Core Animation untagged 8-bit BGRA IOSurfaces
  and has no EDR path (read on GTK main, `gdk/macos/`). There is no HDR
  preview on macOS until GTK gains it, short of a native Metal
  (`CAMetalLayer`, `wantsExtendedDynamicRangeContent`) view, which is a large
  job.
- Impact:
  - The canvas becomes RGBA16F.
  - The grade shader must output an extended range for the HDR rendition
    (see 3).
  - Scopes (histogram, waveform) need a stop or nits scale above SDR white.
  - The clipping overlay must clip at the headroom, not at 1.0.
  - `display_color` stays SDR-only.

## 2. HDR export

- Primary format: gain map, ISO 21496-1 ("Digital photography — gain map
  metadata for image conversion"), published July 2025. Adobe's gain map spec
  became Google Ultra HDR (Android 14+), Apple Adaptive HDR (iOS 18) and the
  ISO standard.
  - Readers: Chrome and Safari 26 read it; Firefox does not (Sept 2026).
  - A JPEG with a gain map shows its SDR base everywhere else, so it gives
    the widest compatibility.
  - Write both ISO 21496-1 and Ultra HDR v1 (XMP) metadata, as Android 15
    does.
- Rust libraries, to evaluate before choosing:
  - `ultrahdr-rs` / `ultrahdr-core` (imazen): pure Rust, follows libultrahdr.
    The core crate has no codec dependency.
  - `ultrahdr`: safe bindings to Google's libultrahdr, the reference
    implementation. One report says the pinned 2.0.0 rejects per-channel
    gain-map coefficients with XMP on.
  - `gainforge`: gain-map maths, by moxcms's author. moxcms is already a
    dependency.
- Second wave, all unverified, research before starting:
  - AVIF or JPEG XL with PQ/HLG, or with an ISO 21496-1 gain map.
  - PNG 16-bit with a `cICP` chunk (PNG third edition, PQ).
  - OpenEXR float (`exr` crate) for external pipelines.
- Metadata to write:
  - Gain map: min/max, gamma, offsets, base and alternate headroom.
  - PQ files: CLLI and MDCV.
  - The ICC or CICP of the base image.
- Export already runs in strips. Gain maps are usually 1/4 resolution, so the
  map can be computed per strip, downsampled.

## 3. The HDR rendition (the core of the work)

Recommended approach, the Adobe and Apple model:

- The SDR rendition stays exactly as today: develop, then grade, then LUTs.
- An HDR rendition of the same look is added. The scene-linear develop goes
  through a tone curve parametrised by headroom: +1…+4 stops over SDR white,
  or the display's peak. Candidates are the ACES 2.0 output transform
  (parametric peak luminance) and darktable's sigmoid or AgX with a variable
  target white. Port one, don't invent one.
- Gain map = log2(HDR / SDR) per pixel, encoded per ISO 21496-1.
- To keep the look identical: compute the gain from the ratio of the HDR tone
  curve to the SDR tone curve, both before the grade, and apply it over the
  graded SDR. Grade and LUTs stay untouched, and the HDR only extends the
  highlights. This is how camera pipelines make Ultra HDR from an SDR edit.
- Log profiles: a V-Log or F-Log signal already carries the HDR range, so its
  HDR rendition is natural.
- UI:
  - An HDR switch and a Headroom slider, in the Photo tab (they are not in
    the LUT).
  - An SDR/HDR toggle in the preview.
  - Export picks "HDR (gain map)".
- Audit cases for `core/src/audit.rs`:
  - `SDR × gain ≈ HDR` round trip within the map's quantisation.
  - PQ encode/decode exactness.
  - Headroom 0 gives a bit-exact SDR.
  - The gain is monotone in the headroom.

## 4. Bracket merge (phase 9)

- State of the art: merge in the RAW domain, before demosaic:
  - align the frames (GPU);
  - weight them noise-optimally per photosite (Granados et al. 2010;
    Hasinoff et al. 2016, HDR+);
  - deghost moving areas.
  - Lightroom and darktable output a float DNG.
- For RawVibes the merged float mosaic is a `RawData` like any other: highlight
  reconstruction, demosaic and NR run unchanged. It needs a multi-file loader
  and alignment; memory grows N×.

## Proposed order

1. Done: HDR rendition and Ultra HDR / ISO 21496-1 JPEG export.
2. HDR preview on Linux (float16 + `GdkColorState`, `GDK_DEBUG=color-mgmt`),
   tested on the user's HDR monitor.
3. AVIF, JPEG XL, PNG PQ.
4. Bracket merge.

## Sources

- ISO 21496-1 overview: https://jackchou00.com/en/posts/iso-21496-1-gainmap-hdr/
- Google and Apple on one standard: https://www.androidauthority.com/google-apple-hdr-photo-standard-3495035/
- Ultra HDR: https://en.wikipedia.org/wiki/Ultra_HDR
- Firefox status: https://connect.mozilla.org/t5/ideas/support-iso-21496-1-the-hdr-over-jpg-standard-via-gain-map-with/idi-p/77844
- HDR on the web, 2026: https://www.inverity.ai/posts/hdr-wide-gamut-images-web
- GTK colour states: https://docs.gtk.org/gdk4/type_func.ColorState.get_rec2100_pq.html, https://blogs.gnome.org/gtk/2024/08/11/the-colors-of-gtk/
- GTK opt-in reports: https://github.com/rusmikev/CineHDR, https://github.com/sukanka/tsukimi/pull/40
- Rust gain-map crates: https://github.com/imazen/ultrahdr, https://docs.rs/ultrahdr/latest/ultrahdr/, https://docs.rs/gainforge/latest/gainforge/
