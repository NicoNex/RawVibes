import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// The LUTs applied after the grade, top to bottom, and how they combine; the browser to find more.
ColumnLayout {
    id: lk
    readonly property var s: Store.s
    readonly property var looks: s.looks || []
    property bool docked: false
    spacing: 8
    Caption {
        visible: lk.looks.length === 0
        Layout.margins: 8
        text: Store.tr("No LUTs: the grade is exported as is.")
        horizontalAlignment: Text.AlignHCenter
    }
    // How looks combine: shown with two or more, and while Mix is on so it can be switched back.
    ColumnLayout {
        Layout.fillWidth: true
        visible: lk.looks.length > 1 || s.lookBlend === "Mix"
        Toggles {
            Layout.fillWidth: true
            model: [Store.tr("Chain"), Store.tr("Mix")]
            current: s.lookBlend === "Mix" ? 1 : 0
            onPicked: (i) => Store.cmd("lookBlend", { blend: i === 1 ? "Mix" : "Chain" })
        }
        Caption { text: (Store.c.blendHint || ["", ""])[s.lookBlend === "Mix" ? 1 : 0] }
    }
    // By count: rows live as long as the stack keeps its length, so a slider is never rebuilt under the pointer.
    Repeater {
        model: lk.looks.length
        LayerRow {
            required property int index
            entry: lk.looks[index] || ({})
            row: index
            count: lk.looks.length
            which: "look"
            flash: s.flashLook === index
        }
    }
    RowLayout {
        visible: !lk.docked
        Layout.alignment: Qt.AlignHCenter
        Btn {
            text: Store.tr("Browse LUTs")
            icon.source: "qrc:/icons/view-grid-symbolic.svg"
            icon.color: Theme.text
            checkable: true
            checked: s.browsing === true
            ToolTip.visible: hovered; ToolTip.text: Store.tr("Browse the LUT library on this photo (B)"); ToolTip.delay: 600
            onClicked: Store.cmd("browse", { on: !s.browsing })
        }
        Icon { name: "document-open"; tip: Store.tr("Add a LUT from a file"); onClicked: Store.cmd("layerAdd", { which: "look", kind: "file" }) }
    }
    Caption { visible: !lk.docked; text: Store.tr("Applied after the grade, top to bottom, and included in the exported LUT.") }
}
