import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Every keyboard shortcut, grouped as in the GTK interface.
DialogShell {
    id: dlg
    // The window's shortcut table: each entry's `shown` keys and `label`.
    property var table: []
    title: Store.tr("Keyboard Shortcuts")
    width: 560
    height: Math.min(640, parent ? parent.height - 40 : 640)
    standardButtons: Dialog.Close
    ListView {
        anchors.fill: parent
        clip: true
        WheelScroll {}
        model: dlg.table.length
        delegate: RowLayout {
            required property int index
            width: ListView.view.width
            height: 30
            Label { text: dlg.table[index].label; color: Theme.text; Layout.fillWidth: true }
            Label {
                text: dlg.table[index].shown
                color: Theme.text; font.pixelSize: Theme.small; font.weight: Font.DemiBold
                padding: 3; leftPadding: 8; rightPadding: 8
                background: Rectangle { color: Theme.raised; radius: 4; border.color: Theme.line }
            }
        }
        ScrollBar.vertical: ScrollBar {}
    }
}
