//! Tiny persisted preferences under `$XDG_CONFIG_HOME/rawvibes/`.
// ponytail: one file per value; switch to GSettings once there are many.

use std::path::{Path, PathBuf};

use relm4::gtk::glib;

/// The LUT library folder.
pub const LUT_FOLDER: &str = "lut-folder";
/// Folder of the last opened photo, where "Open Photo" starts.
pub const PHOTO_FOLDER: &str = "photo-folder";

fn file(name: &str) -> PathBuf {
    glib::user_config_dir().join("rawvibes").join(name)
}

/// The filmstrip when RawVibes closed, one path per line, and the photo open then.
pub const ROLL: &str = "roll";
pub const LAST_PHOTO: &str = "last-photo";

/// "1": RAWs without edits open as in camera.
pub const AS_IN_CAMERA: &str = "as-in-camera";

/// Preview quality while editing: "auto", "fast", "balanced" or "best".
pub const PREVIEW_QUALITY: &str = "preview-quality";

/// "0": no full-quality frame after development controls rest.
pub const REFINE_IDLE: &str = "refine-idle";

/// The canvas background: "default", "black" or "white".
pub const BACKGROUND: &str = "background";

/// "0": the scopes scroll with the sidebar instead of staying pinned on top.
pub const SCOPES_PINNED: &str = "scopes-pinned";

/// Height of the scopes in logical pixels.
pub const SCOPES_HEIGHT: &str = "scopes-height";

/// The stored value; until RawVibes stores its own, VibingLUT's (same keys).
pub fn value(key: &str) -> Option<String> {
    let vibinglut = glib::user_config_dir().join("vibinglut").join(key);
    let text = std::fs::read_to_string(file(key)).or_else(|_| std::fs::read_to_string(vibinglut)).ok()?;
    Some(text.trim().to_owned())
}

pub fn set_value(key: &str, value: &str) -> std::io::Result<()> {
    let f = file(key);
    std::fs::create_dir_all(f.parent().expect("config file has a parent"))?;
    std::fs::write(f, value)
}

pub fn folder(key: &str) -> Option<PathBuf> {
    Some(PathBuf::from(value(key)?)).filter(|p| p.is_dir())
}

pub fn set_folder(key: &str, dir: &Path) -> std::io::Result<()> {
    set_value(key, &dir.to_string_lossy())
}

/// Where presets are saved (`rawvibes_core::preset` JSON files).
pub fn presets_dir() -> PathBuf {
    file("presets")
}

/// Last photo export format (`PhotoFormat::tag`).
pub const EXPORT_FORMAT: &str = "export-format";

/// Last JPEG export quality, 1–100.
pub const JPEG_QUALITY: &str = "jpeg-quality";

/// What exported photos carry (`rawvibes_core::meta::Policy::tag`).
pub const METADATA_POLICY: &str = "metadata-policy";

/// Appended to an exported photo's name (`_edit` when empty).
pub const EXPORT_SUFFIX: &str = "export-suffix";

/// The author defaults stamped on exports whose photo leaves them empty.
pub const AUTHOR: &str = "author";
pub const COPYRIGHT: &str = "copyright";
pub const AUTHOR_EMAIL: &str = "author-email";
pub const AUTHOR_WEBSITE: &str = "author-website";

/// Width of the editor's sidebar, in pixels, as last dragged.
pub const SIDEBAR_WIDTH: &str = "sidebar-width";

/// The filmstrip under the photo is collapsed ("1").
pub const FILMSTRIP_HIDDEN: &str = "filmstrip-hidden";
