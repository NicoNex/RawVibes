import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// A setting that is on or off: title, optional subtitle, and the switch.
RowLayout {
    id: r
    property string title
    property string subtitle
    property bool checked
    signal toggled(bool on)
    Layout.fillWidth: true
    spacing: 8
    opacity: enabled ? 1 : 0.4
    ColumnLayout {
        Layout.fillWidth: true
        spacing: 1
        Label { text: r.title; color: Theme.text; font.pixelSize: Theme.body; wrapMode: Text.Wrap; Layout.fillWidth: true }
        Label {
            visible: r.subtitle !== ""
            text: r.subtitle; color: Theme.dim; font.pixelSize: Theme.small
            wrapMode: Text.Wrap; Layout.fillWidth: true
        }
    }
    Switch {
        id: sw
        checked: r.checked
        Accessible.name: r.title
        onToggled: r.toggled(checked)
        indicator: Rectangle {
            implicitWidth: 34; implicitHeight: 18; radius: 9
            x: sw.leftPadding; y: sw.height / 2 - height / 2
            color: sw.checked ? Theme.accent : Theme.line
            border.color: sw.visualFocus ? Theme.text : "transparent"
            Rectangle {
                x: sw.checked ? parent.width - width - 2 : 2; y: 2
                width: 14; height: 14; radius: 7
                color: sw.checked ? Theme.accentInk : Theme.text
                Behavior on x { NumberAnimation { duration: 100 } }
            }
        }
    }
}
