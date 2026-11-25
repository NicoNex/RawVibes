import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// A segmented choice: one of `model` (strings), `current` its index.
Rectangle {
    id: seg
    property var model: []
    property int current: 0
    signal picked(int index)
    implicitHeight: 28
    implicitWidth: row.implicitWidth + 4
    radius: Theme.radius
    color: Theme.canvas
    border.color: Theme.line
    RowLayout {
        id: row
        anchors { fill: parent; margins: 2 }
        spacing: 2
        Repeater {
            model: seg.model
            AbstractButton {
                Layout.fillWidth: true
                Layout.fillHeight: true
                checked: index === seg.current
                focusPolicy: Qt.StrongFocus
                Accessible.name: modelData
                onClicked: seg.picked(index)
                contentItem: Label {
                    text: modelData
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                    color: parent.checked ? Theme.accentInk : Theme.dim
                    font.pixelSize: Theme.small + 1
                    font.weight: parent.checked ? Font.DemiBold : Font.Normal
                    leftPadding: 8; rightPadding: 8
                }
                background: Rectangle {
                    radius: Theme.radius - 2
                    color: parent.checked ? Theme.accent : (parent.hovered ? Theme.raised : "transparent")
                    border.color: parent.visualFocus ? Theme.text : "transparent"
                }
            }
        }
    }
}
