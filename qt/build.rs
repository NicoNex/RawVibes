use cxx_qt_build::{CxxQtBuilder, QResource, QResources, QmlFile, QmlModule};
use qt_build_utils::QResourceFile;

/// `qmake -query <var>`, with the qmake cxx-qt-build would find ($QMAKE, else qmake6, else qmake).
fn qmake(var: &str) -> String {
    let candidates = std::env::var("QMAKE").into_iter().chain(["qmake6".into(), "qmake".into()]);
    candidates
        .filter_map(|q| std::process::Command::new(q).args(["-query", var]).output().ok())
        .find(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .expect("qmake (Qt 6) on PATH or in $QMAKE")
}

fn main() {
    // The RHI (rhi/qrhi.h, for the HDR swapchain's info) is semi-public: in QtGui's versioned folder.
    let version = qmake("QT_VERSION");
    let rhi = [
        format!("{}/QtGui/{version}/QtGui", qmake("QT_INSTALL_HEADERS")),
        // macOS framework builds.
        format!("{}/QtGui.framework/Headers/{version}/QtGui", qmake("QT_INSTALL_LIBS")),
    ]
    .into_iter()
    .find(|d| std::path::Path::new(d).join("rhi/qrhi.h").exists())
    .expect("Qt's RHI headers (rhi/qrhi.h)");
    let qml = std::fs::read_dir("qml")
        .expect("qml/ exists")
        .map(|e| e.expect("readable qml/").path())
        .filter(|p| p.extension().is_some_and(|e| e == "qml"))
        .map(|p| {
            let singleton = p.file_stem().is_some_and(|s| s == "Theme" || s == "Store");
            QmlFile::from(p).singleton(singleton)
        })
        .collect::<Vec<_>>();
    // Symbolic icons as resources: Windows and macOS have no icon theme to find them in.
    // RawVibes' own icons from data/icons, the theme's from icons/.
    let icons = ["icons", "../data/icons/hicolor/scalable/actions"]
        .iter()
        .flat_map(|d| std::fs::read_dir(d).unwrap_or_else(|e| panic!("{d}: {e}")))
        .map(|e| e.expect("readable icon folder").path())
        .filter(|p| p.extension().is_some_and(|e| e == "svg"))
        .map(|p| {
            let name = p.file_name().expect("a file").to_string_lossy().into_owned();
            QResourceFile::new(&p).alias(format!("icons/{name}"))
        })
        .collect::<Vec<_>>();
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=../data/icons");
    println!("cargo:rerun-if-changed=qml");
    println!("cargo:rerun-if-changed=cpp");
    CxxQtBuilder::new_qml_module(QmlModule::new("RawVibes").qml_files(qml))
        .qt_module("Gui")
        .qt_module("Quick")
        .qt_module("QuickControls2")
        .qrc_resources(QResources::new().resource(QResource::new().prefix("/").files(icons)))
        .include_dir("cpp")
        .include_dir(&rhi)
        .cpp_file("cpp/images.cpp")
        .cpp_file("cpp/hdrwindow.h")
        .files(["src/bridge.rs"])
        .build();
}
