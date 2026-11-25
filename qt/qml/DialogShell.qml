import QtQuick
import QtQuick.Controls
import RawVibes

// Every dialog's frame: modal, centred, the panel colour, a heading.
Dialog {
    modal: true
    anchors.centerIn: Overlay.overlay
    background: Rectangle { color: Theme.panel; radius: 12; border.color: Theme.line }
    header: Label {
        text: parent ? parent.title : ""
        visible: text !== ""
        color: Theme.text; font.pixelSize: 17; font.weight: Font.DemiBold
        padding: 20; bottomPadding: 8
    }
}
