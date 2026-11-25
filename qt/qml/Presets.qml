import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Presets: saved grades to apply to any photo.
Icon {
    id: btn
    name: "tag-outline"
    tip: Store.tr("Presets")
    onClicked: pop.open()
    Popup {
        id: pop
        y: btn.height + 4
        x: btn.width - width
        width: 300
        padding: 12
        background: Rectangle { color: Theme.panel; radius: 12; border.color: Theme.line }
        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            Label { text: Store.tr("Presets"); color: Theme.text; font.pixelSize: Theme.heading; font.weight: Font.DemiBold }
            Caption {
                visible: (Store.s.presets || []).length === 0
                text: Store.tr("No presets yet. Save the current grade to reuse it on other photos.")
            }
            ListView {
                Layout.fillWidth: true
                Layout.preferredHeight: Math.min(300, contentHeight)
                clip: true
                model: (Store.s.presets || []).length
                delegate: ItemDelegate {
                    required property int index
                    readonly property var preset: Store.s.presets[index] || {}
                    width: ListView.view.width
                    text: preset.name || ""
                    onClicked: { Store.cmd("presetApply", { path: preset.path }); pop.close() }
                    Icon {
                        anchors { right: parent.right; verticalCenter: parent.verticalCenter }
                        name: "user-trash"; size: 14
                        tip: Store.tr("Remove")
                        onClicked: Store.cmd("presetDelete", { path: parent.preset.path })
                    }
                }
            }
            Btn {
                Layout.fillWidth: true
                text: Store.tr("Save Current Grade…")
                highlighted: true
                ToolTip.visible: hovered; ToolTip.text: Store.tr("Save the grade as a preset (Ctrl+S)"); ToolTip.delay: 600
                onClicked: { Store.cmd("presetSave"); pop.close() }
            }
            RowLayout {
                Layout.fillWidth: true
                Btn { Layout.fillWidth: true; flat: true; text: Store.tr("Import…"); onClicked: { Store.cmd("presetImport"); pop.close() } }
                Btn { Layout.fillWidth: true; flat: true; text: Store.tr("Export…"); onClicked: { Store.cmd("presetExport"); pop.close() } }
            }
        }
    }
}
