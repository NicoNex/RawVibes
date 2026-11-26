import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Shapes
import QtCore
import RawVibes

// Histogram, waveform or vectorscope of the graded photo; a grip underneath sets the height.
ColumnLayout {
    id: sc
    readonly property var s: Store.s
    property int kind: 0
    spacing: 6
    Settings { id: saved; category: "scopes"; property int height: 110; property int kind: 0 }
    Component.onCompleted: kind = saved.kind
    RowLayout {
        Layout.fillWidth: true
        Toggles {
            Layout.fillWidth: true
            model: [Store.tr("Histogram"), Store.tr("Waveform"), Store.tr("Vectorscope")]
            current: sc.kind
            onPicked: (i) => { sc.kind = i; saved.kind = i }
        }
        Icon {
            visible: sc.kind === 2
            name: "avatar-default"
            tip: Store.tr("Skin tone line (K)")
            checkable: true
            checked: s.skin
            onClicked: Store.cmd("skin", { on: !s.skin })
        }
    }
    Rectangle {
        Layout.fillWidth: true
        Layout.preferredHeight: saved.height
        color: Theme.canvas
        radius: Theme.radius
        clip: true
        Image {
            anchors.fill: parent
            anchors.margins: 1
            source: Store.img(["hist", "wave", "vec"][sc.kind])
            fillMode: sc.kind === 2 ? Image.PreserveAspectFit : Image.Stretch
            cache: false
            retainWhileLoading: true
            // The skin tone line: 123° from +Cb, between red and yellow; skin of any ethnicity lands on it.
            Shape {
                visible: sc.kind === 2 && s.skin
                anchors.centerIn: parent
                width: Math.min(parent.paintedWidth, parent.paintedHeight); height: width
                ShapePath {
                    strokeColor: Qt.rgba(1, 0.82, 0.62, 0.9); strokeWidth: 1.5; strokeStyle: ShapePath.DashLine
                    dashPattern: [3, 2]; fillColor: "transparent"
                    startX: width / 2; startY: height / 2
                    PathLine { x: width / 2 + Math.cos(123 * Math.PI / 180) * width / 2; y: height / 2 - Math.sin(123 * Math.PI / 180) * height / 2 }
                }
            }
        }
    }
    Rectangle {
        Layout.fillWidth: true
        height: 8
        color: "transparent"
        Rectangle { anchors.centerIn: parent; width: 36; height: 3; radius: 1.5; color: grip.containsMouse || grip.pressed ? Theme.dim : Theme.line }
        MouseArea {
            id: grip
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.SizeVerCursor
            property real y0
            property int h0
            onPressed: (e) => { y0 = mapToGlobal(e.x, e.y).y; h0 = saved.height }
            onPositionChanged: (e) => { if (pressed) saved.height = Math.max(50, Math.min(320, h0 + mapToGlobal(e.x, e.y).y - y0)) }
        }
        ToolTip.visible: grip.containsMouse && !grip.pressed
        ToolTip.text: Store.tr("Drag to resize")
        ToolTip.delay: 800
    }
}
