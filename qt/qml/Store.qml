pragma Singleton
import QtQuick
import RawVibes

// The editor's state as Rust publishes it, and the one way to change it.
QtObject {
    readonly property var s: JSON.parse(Backend.state || "{}")
    // What never changes while RawVibes runs (names, colours, choices): parsed once.
    readonly property var c: JSON.parse(Backend.constants || "{}")
    // The image provider's frames (key → generation): a new frame touches only the images showing it.
    readonly property var f: JSON.parse(Backend.frames || "{}")
    // The frames each render replaces (photo, hdr, scopes, lab): apart, so thumbnails do not rebind per frame.
    readonly property var lf: JSON.parse(Backend.live || "{}")
    readonly property var liveKeys: ["photo", "hdr", "hist", "wave", "vec", "lab"]
    // The generation of frame `key`, 0 until there is one.
    function gen(key) {
        return (liveKeys.indexOf(key) >= 0 ? lf : f)[key] || 0
    }
    // The Metadata tab's link to Preferences.
    signal prefsRequested()
    // A toast is drawn over the photo: the HDR surface, a native window above every Qt Quick item,
    // steps aside for it (PhotoCanvas checks menus, dialogs and tooltips by where they are).
    property bool covered: false

    function cmd(name, args) {
        Backend.cmd(name, JSON.stringify(args === undefined ? {} : args))
    }
    // A file URL as a path: file:///home/x → /home/x, file:///C:/x → C:/x.
    function localPath(url) {
        let p = decodeURIComponent(url.toString().replace(/^file:\/\//, ""))
        return /^\/[A-Za-z]:/.test(p) ? p.substring(1) : p
    }
    // Files dropped on the window: added to the filmstrip, the first opened.
    function dropPaths(urls) {
        cmd("rollAdd", { paths: urls.map(localPath) })
    }
    // The URL of frame `key`, "" until there is one.
    function img(key) {
        const g = gen(key)
        return g ? "image://rv/" + key + "/" + g : ""
    }
    function tr(text) {
        return Backend.tr(text)
    }
}
