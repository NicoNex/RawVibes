import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// One entry of a stack (the looks after the grade, or the LUT Lab): on/off, intensity, the signal it expects, order.
Rectangle {
    id: row
    property var entry
    property int row
    property int count
    property string which: "look"
    property bool flash: false
    Layout.fillWidth: true
    implicitHeight: col.implicitHeight + 16
    radius: Theme.radius
    color: flash ? Qt.rgba(0.88, 0.63, 0.25, 0.25) : Theme.raised
    Behavior on color { ColorAnimation { duration: 600 } }
    function send(name, args) { args.which = row.which; args.i = row.row; Store.cmd(name, args) }
    ColumnLayout {
        id: col
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: 8 }
        spacing: 4
        opacity: entry.on ? 1 : 0.5
        RowLayout {
            Layout.fillWidth: true
            CheckBox {
                checked: entry.on === true
                Accessible.name: (entry.on ? Store.tr("Switch off {}") : Store.tr("Switch on {}")).replace("{}", entry.title || "")
                ToolTip.visible: hovered; ToolTip.text: entry.on ? Store.tr("Switch off") : Store.tr("Switch on"); ToolTip.delay: 600
                onToggled: row.send("layerOn", { on: checked })
            }
            Label {
                text: entry.badge || ""
                color: Theme.dim; font.pixelSize: 9; font.weight: Font.Bold
                padding: 2; leftPadding: 4; rightPadding: 4
                background: Rectangle { color: Theme.canvas; radius: 3 }
            }
            Label { text: entry.title || ""; color: Theme.text; elide: Text.ElideMiddle; Layout.fillWidth: true; font.pixelSize: Theme.body }
            Icon { name: "go-up"; tip: Store.tr("Move up"); enabled: row.row > 0; onClicked: row.send("layerMove", { up: true }) }
            Icon { name: "go-down"; tip: Store.tr("Move down"); enabled: row.row < row.count - 1; onClicked: row.send("layerMove", { up: false }) }
            Icon { name: "user-trash"; tip: Store.tr("Remove"); onClicked: row.send("layerRemove", {}) }
        }
        ValueSlider {
            Layout.fillWidth: true
            visible: entry.kind !== "convert"
            label: Store.tr("Intensity"); from: 0; to: 100; neutral: 100; unit: "%"
            value: entry.intensity
            onMoved: (v) => row.send("layerIntensity", { v: v })
        }
        Caption {
            visible: entry.kind === "grade"
            text: Store.tr("Input: {}").replace("{}", Store.s.sourceProfileName || "")
        }
        RowLayout {
            Layout.fillWidth: true
            visible: entry.kind !== "grade"
            Label {
                text: entry.kind === "convert" ? Store.tr("Convert the signal to") : Store.tr("This LUT was made for:")
                color: Theme.dim; font.pixelSize: Theme.small
            }
            Choice {
                Layout.fillWidth: true
                model: (Store.s.profiles || []).map(p => p.name)
                currentIndex: entry.inputIndex
                onActivated: (i) => row.send("layerProfile", { profile: Store.s.profiles[i].key })
            }
        }
    }
}
