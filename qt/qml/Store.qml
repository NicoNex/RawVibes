pragma Singleton
import QtQuick
import RawVibes

// The editor's state as Rust publishes it, and the one way to change it.
QtObject {
    readonly property var s: JSON.parse(Backend.state || "{}")

    function cmd(name, args) {
        Backend.cmd(name, JSON.stringify(args === undefined ? {} : args))
    }
    function tr(text) {
        return Backend.tr(text)
    }
}
