import QtQuick
import QtQuick.Controls
import QtQuick.Controls.impl
import QtQuick.Layouts
import RawVibes

// A collapsible group of controls. `eye` names the part it switches off (see command.rs `off_key`);
// switched off, its controls dim and the photo shows it neutral.
ColumnLayout {
    id: sec
    property string title
    property string eye: ""
    property bool open: true
    property alias header: extra.data
    default property alias content: body.data
    readonly property bool off: eye !== "" && (Store.s.off || []).indexOf(eye) >= 0
    Layout.fillWidth: true
    spacing: 0

    Rectangle {
        Layout.fillWidth: true
        implicitHeight: 36
        radius: Theme.radius
        color: hover.hovered ? Theme.raised : "transparent"
        HoverHandler { id: hover }
        TapHandler { onTapped: sec.open = !sec.open }
        RowLayout {
            anchors { fill: parent; leftMargin: 8; rightMargin: 4 }
            spacing: 2
            IconImage {
                source: "qrc:/icons/pan-" + (sec.open ? "down" : "end") + "-symbolic.svg"
                sourceSize: Qt.size(12, 12)
                color: Theme.dim
            }
            Label {
                text: sec.title
                color: sec.off ? Theme.faint : Theme.text
                font.pixelSize: Theme.heading
                font.weight: Font.DemiBold
                Layout.fillWidth: true
                Layout.leftMargin: 4
                Accessible.role: Accessible.Button
                Accessible.name: sec.title
            }
            Row { id: extra; spacing: 2 }
            Icon {
                visible: sec.eye !== ""
                name: sec.off ? "view-conceal" : "view-reveal"
                tip: sec.off ? Store.tr("Enable section") : Store.tr("Disable section")
                onClicked: Store.cmd("off", { key: sec.eye, off: !sec.off })
            }
        }
    }
    ColumnLayout {
        id: body
        visible: sec.open
        opacity: sec.off ? 0.45 : 1
        Layout.fillWidth: true
        Layout.leftMargin: 8
        Layout.rightMargin: 8
        Layout.topMargin: 2
        Layout.bottomMargin: 10
        spacing: 6
    }
}
