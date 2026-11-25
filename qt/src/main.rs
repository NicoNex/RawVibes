#![recursion_limit = "512"]
//! RawVibes with a Qt 6 / QML interface, alongside the GTK one: the same core and edits.

mod app;
mod bridge;
mod i18n;
mod settings;
mod timers;
mod view;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};
use cxx_qt_lib::QQuickStyle;

fn main() {
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
