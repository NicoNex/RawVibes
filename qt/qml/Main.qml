import QtQuick
import QtQuick.Controls
import RawVibes

ApplicationWindow {
    id: win
    width: 1400; height: 900
    visible: true
    title: Store.s.title + " — RawVibes"
    color: Theme.canvas
    Label { anchors.centerIn: parent; text: Store.s.info || "RawVibes"; color: Theme.text }
}
