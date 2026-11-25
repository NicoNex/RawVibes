import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Every keyboard shortcut, grouped as in the GTK interface.
Dialog {
    id: dlg
    title: Store.tr("Keyboard Shortcuts")
    modal: true
    anchors.centerIn: Overlay.overlay
    width: 560
    height: Math.min(640, parent ? parent.height - 40 : 640)
    standardButtons: Dialog.Close
    background: Rectangle { color: Theme.panel; radius: 12; border.color: Theme.line }
    header: Label { text: dlg.title; color: Theme.text; font.pixelSize: 17; font.weight: Font.DemiBold; padding: 20; bottomPadding: 8 }
    readonly property var keys: [
        ["Ctrl+O", Store.tr("Open Photo")], ["Ctrl+Z", Store.tr("Undo")], ["Ctrl+Shift+Z", Store.tr("Redo")],
        ["Ctrl+Shift+C", Store.tr("Copy edits")], ["Ctrl+Shift+V", Store.tr("Paste edits")],
        ["1–5", Store.tr("Stars")], ["0", Store.tr("No stars")], ["6–9", Store.tr("Colour label: red, yellow, green, blue")],
        ["F11", Store.tr("Fullscreen")], ["Ctrl+E", Store.tr("Export photo")], ["Ctrl+Shift+E", Store.tr("Export LUT · 33³ .cube")],
        ["\\", Store.tr("Before / after")], ["T", Store.tr("Test chart")], ["C", Store.tr("Show clipping")],
        ["K", Store.tr("Skin tone line")], ["G", Store.tr("Grid of all photos")], ["R", Store.tr("Edit crop")],
        ["Esc", Store.tr("Cancel crop")], ["Enter", Store.tr("Apply crop")], ["Ctrl+0", Store.tr("Zoom to fit")],
        ["Ctrl+1", Store.tr("Actual size")], ["Ctrl++", Store.tr("Zoom in")], ["Ctrl+−", Store.tr("Zoom out")],
        ["Ctrl+S", Store.tr("Save preset")], ["Alt+1", Store.tr("Photo view")], ["Alt+2", Store.tr("Grade view")],
        ["Alt+3", Store.tr("Metadata view")], ["Alt+4", Store.tr("LUT Lab view")], ["B", Store.tr("Browse LUTs")],
        ["Ctrl+F", Store.tr("Search LUTs")], ["F5", Store.tr("Reload LUTs")], ["Ctrl+,", Store.tr("Preferences")],
        ["Ctrl+?", Store.tr("Keyboard Shortcuts")]
    ]
    ListView {
        anchors.fill: parent
        clip: true
        model: dlg.keys.length
        delegate: RowLayout {
            required property int index
            width: ListView.view.width
            height: 30
            Label { text: dlg.keys[index][1]; color: Theme.text; Layout.fillWidth: true }
            Label {
                text: dlg.keys[index][0]
                color: Theme.text; font.pixelSize: Theme.small; font.weight: Font.DemiBold
                padding: 3; leftPadding: 8; rightPadding: 8
                background: Rectangle { color: Theme.raised; radius: 4; border.color: Theme.line }
            }
        }
        ScrollBar.vertical: ScrollBar {}
    }
}
