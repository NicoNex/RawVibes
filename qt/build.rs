use cxx_qt_build::{CxxQtBuilder, QResource, QResources, QmlFile, QmlModule};
use qt_build_utils::QResourceFile;

fn main() {
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
    let icons = std::fs::read_dir("icons")
        .expect("icons/ exists")
        .map(|e| e.expect("readable icons/").path())
        .filter(|p| p.extension().is_some_and(|e| e == "svg"))
        .map(|p| {
            let name = p.file_name().expect("a file").to_string_lossy().into_owned();
            QResourceFile::new(&p).alias(format!("icons/{name}"))
        })
        .collect::<Vec<_>>();
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=qml");
    println!("cargo:rerun-if-changed=cpp");
    CxxQtBuilder::new_qml_module(QmlModule::new("RawVibes").qml_files(qml))
        .qt_module("Gui")
        .qt_module("Quick")
        .qt_module("QuickControls2")
        .qrc_resources(QResources::new().resource(QResource::new().prefix("/").files(icons)))
        .include_dir("cpp")
        .cpp_file("cpp/images.cpp")
        .files(["src/bridge.rs"])
        .build();
}
