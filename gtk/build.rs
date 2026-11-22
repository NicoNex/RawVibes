// Bundles the relm4-icons the UI uses into a gresource; the rest of the set stays out of the binary.
fn main() {
    relm4_icons_build::bundle_icons(
        "icon_names.rs",
        Some("io.github.rawvibes.RawVibes"),
        None::<&str>,
        None::<&str>,
        // A name in several sets resolves to whichever the crate finds first, so these are unique
        // or alike in every set ("color" is the GNOME colour discs in both GNOME kits).
        ["magic-wand", "camera-shutter", "color", "tag-outline"],
    );
}
