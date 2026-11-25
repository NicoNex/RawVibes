pragma Singleton
import QtQuick
import RawVibes

// The editor's state as Rust publishes it, and the one way to change it.
QtObject {
    readonly property var s: JSON.parse(Backend.state || "{}")
    // Interface-only state: straightening along a line drawn on the photo.
    property bool levelling: false
    // The Metadata tab's link to Preferences.
    signal prefsRequested()

    function cmd(name, args) {
        Backend.cmd(name, JSON.stringify(args === undefined ? {} : args))
    }
    // A file URL as a path: file:///home/x → /home/x, file:///C:/x → C:/x.
    function localPath(url) {
        let p = decodeURIComponent(url.toString().replace(/^file:\/\//, ""))
        return /^\/[A-Za-z]:/.test(p) ? p.substring(1) : p
    }
    function tr(text) {
        return Backend.tr(text)
    }
}
