//! gettext, with the GTK UI's `rawvibes` catalogues: one translation for both interfaces.

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

/// `gettext!("… {} …", a, b)`: the translation with `{}` filled in order.
#[macro_export]
macro_rules! gettext {
    ($msgid:literal, $($arg:expr),+ $(,)?) => {{
        let args = [$($arg.to_string()),+];
        $crate::i18n::fill(&$crate::i18n::gettext($msgid), &args)
    }};
}

/// The interface's translation of `msgid`, or `msgid` itself.
pub fn gettext(msgid: &str) -> String {
    let Ok(id) = CString::new(msgid) else { return msgid.to_owned() };
    // SAFETY: gettext returns a NUL-terminated static translation, or `id`, which outlives the copy.
    unsafe { CStr::from_ptr(gettext_sys::gettext(id.as_ptr())) }.to_string_lossy().into_owned()
}

pub fn fill(template: &str, args: &[String]) -> String {
    let mut parts = template.split("{}");
    let mut out = parts.next().unwrap_or_default().to_string();
    for (part, arg) in parts.zip(args.iter().map(String::as_str).chain(std::iter::repeat(""))) {
        out += arg;
        out += part;
    }
    out
}

/// Points gettext at the `rawvibes` catalogues; a missing one leaves the English source strings.
pub fn bind(dir: &Path) {
    let domain = c"rawvibes";
    let Ok(dir) = CString::new(dir.as_os_str().as_encoded_bytes()) else { return };
    // SAFETY: plain C calls with NUL-terminated strings that gettext copies; LC_ALL from the environment.
    unsafe {
        libc_setlocale();
        gettext_sys::bindtextdomain(domain.as_ptr(), dir.as_ptr());
        gettext_sys::bind_textdomain_codeset(domain.as_ptr(), c"UTF-8".as_ptr());
        gettext_sys::textdomain(domain.as_ptr());
    }
}

/// GTK calls setlocale itself; Qt does too, but only once QGuiApplication exists.
unsafe fn libc_setlocale() {
    unsafe extern "C" {
        fn setlocale(category: i32, locale: *const std::ffi::c_char) -> *mut std::ffi::c_char;
    }
    // LC_ALL is 6 on glibc, 0 on macOS and Windows.
    const LC_ALL: i32 = if cfg!(target_os = "linux") { 6 } else { 0 };
    // SAFETY: the empty string selects the environment's locale; called before any other thread starts.
    unsafe { setlocale(LC_ALL, c"".as_ptr()) };
}

/// Installed: `<prefix>/bin/…` → `<prefix>/share/locale`; dev: `make` puts catalogues in `target/locale`.
pub fn locale_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/locale")))
        .filter(|d| d.join("it/LC_MESSAGES/rawvibes.mo").exists())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/locale")))
}
