import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// A labelled slider: its name, its value (click to type one), and a fill from the neutral
// point to the value. A double-click puts it back to neutral.
Item {
    id: root
    property string label
    property real from: -100
    property real to: 100
    property real value: 0
    property real neutral: from < 0 && to > 0 ? 0 : from
    property int decimals: 0
    property string unit: ""
    property real step: decimals > 0 ? Math.pow(10, -decimals) : 1
    // Gradient stops drawn in the groove (temperature, hue bands…) instead of the fill.
    property var track: []
    signal moved(real value)

    implicitHeight: 38
    implicitWidth: 240
    opacity: enabled ? 1 : 0.4

    function shown(v) { return Number(v).toFixed(decimals) + unit }
    function send(v) {
        v = Math.max(from, Math.min(to, v))
        if (v !== value) root.moved(v)
    }

    RowLayout {
        id: head
        anchors { left: parent.left; right: parent.right; top: parent.top }
        height: 18
        Label {
            text: root.label
            color: slider.hovered || slider.pressed ? Theme.text : Theme.dim
            font.pixelSize: Theme.body
            elide: Text.ElideRight
            Layout.fillWidth: true
        }
        // Shown once the slider has moved off neutral, as in the GTK interface.
        Icon {
            name: "edit-undo"; size: 12
            implicitWidth: 18; implicitHeight: 18
            tip: Store.tr("Reset")
            Accessible.name: Store.tr("Reset") + " " + root.label
            // Hidden by opacity, not visibility: the label never shifts.
            readonly property bool moved: Math.abs(root.value - root.neutral) > 1e-9
            opacity: moved ? 1 : 0
            enabled: moved
            onClicked: root.send(root.neutral)
        }
        TextInput {
            id: entry
            text: root.shown(root.value)
            color: slider.pressed ? Theme.accent : Theme.text
            font.pixelSize: Theme.body
            font.features: { "tnum": 1 }
            horizontalAlignment: TextInput.AlignRight
            selectByMouse: true
            Layout.preferredWidth: 64
            Accessible.name: root.label
            onAccepted: { root.send(parseFloat(text.replace(",", ".")) || 0); focus = false }
            onActiveFocusChanged: if (activeFocus) selectAll(); else text = Qt.binding(() => root.shown(root.value))
            Keys.onEscapePressed: focus = false
        }
    }

    Slider {
        id: slider
        anchors { left: parent.left; right: parent.right; bottom: parent.bottom }
        height: 18
        from: root.from; to: root.to
        value: root.value
        stepSize: root.step
        live: true
        focusPolicy: Qt.StrongFocus
        Accessible.name: root.label
        onMoved: root.send(value)

        background: Item {
            x: slider.leftPadding; width: slider.availableWidth
            y: slider.topPadding + slider.availableHeight / 2 - 2; height: 4
            Rectangle {
                anchors.fill: parent
                radius: 2
                color: Theme.line
                visible: root.track.length === 0
            }
            // The groove's colours: one two-stop segment per pair, so nothing is created per update.
            Row {
                anchors.fill: parent
                visible: root.track.length > 1
                Repeater {
                    model: Math.max(0, root.track.length - 1)
                    Rectangle {
                        required property int index
                        width: parent.width / (root.track.length - 1); height: parent.height
                        gradient: Gradient {
                            orientation: Gradient.Horizontal
                            GradientStop { position: 0; color: root.track[index] || "transparent" }
                            GradientStop { position: 1; color: root.track[index + 1] || "transparent" }
                        }
                    }
                }
            }
            // Neutral → value, so how far a control has moved reads at a glance.
            Rectangle {
                visible: root.track.length === 0
                readonly property real a: (root.neutral - root.from) / (root.to - root.from)
                readonly property real b: slider.visualPosition
                x: Math.min(a, b) * parent.width
                width: Math.abs(b - a) * parent.width
                height: parent.height
                radius: 2
                color: Theme.accent
            }
        }
        handle: Rectangle {
            x: slider.leftPadding + slider.visualPosition * (slider.availableWidth - width)
            y: slider.topPadding + slider.availableHeight / 2 - height / 2
            width: slider.pressed ? 14 : 12; height: width
            radius: width / 2
            color: slider.pressed ? Theme.accent : Theme.text
            border.color: slider.visualFocus ? Theme.accent : Theme.canvas
            border.width: 2
            Behavior on width { NumberAnimation { duration: 80 } }
        }
    }

    TapHandler {
        acceptedButtons: Qt.LeftButton
        onDoubleTapped: root.send(root.neutral)
    }
}
