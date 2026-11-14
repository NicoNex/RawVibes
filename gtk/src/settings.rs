//! Tiny persisted preferences under `$XDG_CONFIG_HOME/rawvalley/`.
// ponytail: one file per value; switch to GSettings once there are many.

use std::path::{Path, PathBuf};

use relm4::gtk::glib;

/// The LUT library folder.
pub const LUT_FOLDER: &str = "lut-folder";
/// Folder of the last opened photo, where "Open Photo" starts.
pub const PHOTO_FOLDER: &str = "photo-folder";

fn file(name: &str) -> PathBuf {
    glib::user_config_dir().join("rawvalley").join(name)
}

/// "1": RAWs without edits open as in camera.
pub const AS_IN_CAMERA: &str = "as-in-camera";

/// Preview quality while editing: "fast", "balanced" or "best".
pub const PREVIEW_QUALITY: &str = "preview-quality";

/// The canvas background: "default", "black" or "white".
pub const BACKGROUND: &str = "background";

/// The stored value; until RawValley stores its own, VibingLUT's (same keys).
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

/// Where presets are saved (`rawvalley_core::preset` JSON files).
pub fn presets_dir() -> PathBuf {
    file("presets")
}
