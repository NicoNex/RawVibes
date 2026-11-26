#![recursion_limit = "512"]
// Windows: a release build opens no console window next to the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! RawVibes with a Qt 6 / QML interface, alongside the GTK one: the same core and edits.

mod app;
mod bridge;
mod hdr;
mod i18n;
mod settings;
mod timers;
mod view;

use cxx_qt_lib::QQuickStyle;
use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};

/// A Vulkan driver is installed (an ICD manifest where the loader looks): Qt Quick can render with it.
#[cfg(target_os = "linux")]
fn vulkan_driver() -> bool {
    let has = |d: &str| std::fs::read_dir(d).is_ok_and(|mut e| e.next().is_some());
    std::env::var_os("VK_ICD_FILENAMES").is_some()
        || ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d", "/usr/local/share/vulkan/icd.d"].into_iter().any(has)
}

fn main() {
    // HDR swapchains exist on Vulkan, Metal and D3D, not OpenGL (Qt Quick's default on Linux);
    // the GPU core already needs Vulkan there.
    #[cfg(target_os = "linux")]
    if std::env::var_os("QSG_RHI_BACKEND").is_none() && vulkan_driver() {
        // SAFETY: first thing in main, before Qt or any other thread starts.
        unsafe { std::env::set_var("QSG_RHI_BACKEND", "vulkan") };
    }
    // Qt Wayland warns on every focus move between the window and the HDR surface's: noise, not a fault.
    if std::env::var_os("QT_LOGGING_RULES").is_none() {
        // SAFETY: before Qt or any other thread starts.
        unsafe { std::env::set_var("QT_LOGGING_RULES", "qt.qpa.wayland.textinput=false") };
    }
    i18n::bind(&i18n::locale_dir());
    let mut app = QGuiApplication::new();
    if let Some(mut app) = app.as_mut() {
        // Where QML's own settings (panel sizes) live: ~/.config/rawvibes/rawvibes-qt.conf and the platform equivalents.
        app.as_mut().set_organization_name(&QString::from("rawvibes"));
        app.as_mut().set_organization_domain(&QString::from("io.github.rawvibes"));
        app.as_mut().set_application_name(&QString::from("rawvibes-qt"));
    }
    QQuickStyle::set_style(&QString::from("Basic"));
    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        bridge::rv_install_images(engine);
    }
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/RawVibes/qml/Main.qml"));
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
