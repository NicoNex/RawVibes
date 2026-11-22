// Bundles the relm4-icons the UI uses into a gresource; the rest of the set stays out of the binary.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    relm4_icons_build::bundle_icons(
        "icon_names.rs",
        Some("io.github.rawvibes.RawVibes"),
        None::<&str>,
        None::<&str>,
        // A name in several sets resolves to whichever the crate finds first: these are in one set only.
        ["magic-wand", "camera-shutter", "tag-outline"],
    );
}
