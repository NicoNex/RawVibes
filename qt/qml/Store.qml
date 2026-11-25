pragma Singleton
import QtQuick
import RawVibes

// The editor's state as Rust publishes it, and the one way to change it.
QtObject {
    readonly property var s: JSON.parse(Backend.state || "{}")
    // What never changes while RawVibes runs (names, colours, choices): parsed once.
    readonly property var c: JSON.parse(Backend.constants || "{}")
    // The Metadata tab's link to Preferences.
    signal prefsRequested()
    // Something is drawn over the photo (a menu, dialog, popup or toast): the HDR surface, a native
    // window above every Qt Quick item, steps aside for it.
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
    function tr(text) {
        return Backend.tr(text)
    }
}
