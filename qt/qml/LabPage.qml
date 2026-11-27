import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// LUT Lab: LUTs, the current grade and profile conversions, chained or mixed into one LUT.
ColumnLayout {
    id: lab
    readonly property var s: Store.s
    readonly property var layers: s.lab || []
    spacing: 8

    Caption { text: Store.tr("To convert a LUT from one profile to another: 1) add the LUT, 2) check which profile it was made for, 3) choose the signal you will use the result on, 4) export.") }

    RowLayout {
        Layout.fillWidth: true
        Layout.topMargin: 6
        Heading { text: Store.tr("1 · Layers"); Layout.fillWidth: true }
        Icon { id: addBtn; name: "list-add"; tip: Store.tr("Add layer"); onClicked: addMenu.popup(addBtn, 0, addBtn.height) }
        Menu {
            id: addMenu
            MenuItem { text: Store.tr("LUT from file…"); onTriggered: Store.cmd("layerAdd", { which: "lab", kind: "file" }) }
            MenuItem { text: Store.tr("LUT selected in the browser"); onTriggered: Store.cmd("layerAdd", { which: "lab", kind: "selected" }) }
            MenuItem { text: Store.tr("Current grade"); onTriggered: Store.cmd("layerAdd", { which: "lab", kind: "grade" }) }
            MenuItem { text: Store.tr("Profile conversion"); onTriggered: Store.cmd("layerAdd", { which: "lab", kind: "convert" }) }
        }
    }
    ColumnLayout {
        visible: lab.layers.length === 0
        Layout.fillWidth: true
        Caption { text: Store.tr("No layers."); horizontalAlignment: Text.AlignHCenter }
        Btn { Layout.alignment: Qt.AlignHCenter; text: Store.tr("Add a LUT…"); highlighted: true; onClicked: Store.cmd("layerAdd", { which: "lab", kind: "file" }) }
    }
    Repeater {
        model: lab.layers.length
        LayerRow {
            required property int index
            entry: lab.layers[index] || ({})
            row: index
            count: lab.layers.length
            which: "lab"
        }
    }

    Heading { text: Store.tr("2 · I will use the final LUT on"); Layout.topMargin: 10 }
    Choice {
        Layout.fillWidth: true
        model: Store.c.profileNames
        currentIndex: Math.max(0, (Store.c.profiles || []).findIndex(p => p.key === s.labInput))
        Accessible.name: Store.tr("Signal the final LUT will be used on")
        onActivated: (i) => Store.cmd("labInput", { profile: Store.c.profiles[i].key })
    }
    Caption { text: Store.tr("The camera profile (standard or log) you will load the exported LUT on. The signal is converted automatically to each layer's profile.") }

    Heading { text: Store.tr("Layer blending"); Layout.topMargin: 10 }
    Toggles {
        Layout.fillWidth: true
        model: [Store.tr("Chain"), Store.tr("Mix")]
        current: s.labBlend === "Mix" ? 1 : 0
        onPicked: (i) => Store.cmd("labBlend", { blend: i === 1 ? "Mix" : "Chain" })
    }
    Caption { text: (Store.c.blendHint || ["", ""])[s.labBlend === "Mix" ? 1 : 0] }
    Label {
        visible: (s.labSummary || "") !== ""
        Layout.fillWidth: true
        text: s.labSummary || ""
        wrapMode: Text.Wrap
        color: Theme.text; font.pixelSize: Theme.small + 1
        padding: 8
        background: Rectangle { color: Theme.canvas; radius: Theme.radius }
    }
}
