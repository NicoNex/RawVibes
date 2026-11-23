//! sRGB preview pixels → the monitor's colour space, where the system does not do it.
//!
//! Wayland compositors with `wp_color_manager_v1` (GNOME ≥ 48, KDE Plasma 6) colour-manage
//! sRGB surfaces themselves, HDR mode included. GTK's macOS backend hands untagged 8-bit
//! surfaces to Core Animation, so on macOS the display's ColorSync profile is applied here
//! with moxcms (relative colorimetric, as darktable and RapidRaw do).

use moxcms::{ColorProfile, Layout, RenderingIntent, Transform8BitExecutor, TransformOptions};
use rawvibes_core::gpu::Done;
use std::sync::{Arc, LazyLock};

static MONITOR: LazyLock<Option<Arc<Transform8BitExecutor>>> =
    LazyLock::new(|| monitor_icc().and_then(|icc| ColorProfile::new_from_slice(&icc).ok()).and_then(|p| from_srgb(&p)));

/// sRGB RGBA8 → `to` RGBA8; `None` when moxcms cannot build the transform.
fn from_srgb(to: &ColorProfile) -> Option<Arc<Transform8BitExecutor>> {
    let options = TransformOptions { rendering_intent: RenderingIntent::RelativeColorimetric, ..Default::default() };
    ColorProfile::new_srgb().create_transform_8bit(Layout::Rgba, to, Layout::Rgba, options).ok()
}

/// `rgba` (sRGB) in the monitor's colour space; unchanged where the system manages colour.
pub fn to_monitor(rgba: Vec<u8>) -> Vec<u8> {
    let Some(t) = MONITOR.as_ref() else { return rgba };
    let mut out = vec![0; rgba.len()];
    // A transform error leaves the pixels as they were: unmanaged, never lost.
    if t.transform(&rgba, &mut out).is_ok() { out } else { rgba }
}

/// `d` with its photo frames in the monitor's colour space; scopes (graphs) and the rest as they are.
pub fn frames_to_monitor(mut d: Done) -> Done {
    if let Done::Graded { rgba, .. }
    | Done::Lab { rgba, .. }
    | Done::Thumb { rgba, .. }
    | Done::RollFrame { rgba, .. } = &mut d
    {
        *rgba = to_monitor(std::mem::take(rgba));
    }
    d
}

// shortcut: the main display's profile, read once; upgrade to the window's display
// (NSWindow.screen) when moving across monitors with different profiles matters.
#[cfg(target_os = "macos")]
fn monitor_icc() -> Option<Vec<u8>> {
    use objc2_core_graphics::{CGColorSpace, CGDisplayCopyColorSpace, CGMainDisplayID};
    CGColorSpace::icc_data(Some(&CGDisplayCopyColorSpace(CGMainDisplayID()))).map(|d| d.to_vec())
}

#[cfg(not(target_os = "macos"))]
fn monitor_icc() -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_red_lands_inside_p3() {
        // Pure sRGB red is a less saturated red in Display P3: ≈ (234, 51, 35).
        let t = from_srgb(&ColorProfile::new_display_p3()).expect("sRGB → P3");
        let mut out = [0u8; 8];
        t.transform(&[255, 0, 0, 255, 128, 128, 128, 7], &mut out).expect("transform");
        let [r, g, b, a] = [out[0], out[1], out[2], out[3]].map(i32::from);
        assert!((r - 234).abs() <= 2 && (g - 51).abs() <= 3 && (b - 35).abs() <= 3, "{out:?}");
        assert_eq!(a, 255);
        // Greys stay grey (same white point and transfer), alpha untouched.
        assert!(out[4..7].iter().all(|&v| (i32::from(v) - 128).abs() <= 1), "{out:?}");
        assert_eq!(out[7], 7);
    }

    #[test]
    fn no_monitor_profile_is_identity() {
        // Linux: the compositor manages colour, the pixels pass through.
        if MONITOR.is_none() {
            assert_eq!(to_monitor(vec![1, 2, 3, 4]), vec![1, 2, 3, 4]);
        }
    }
}
