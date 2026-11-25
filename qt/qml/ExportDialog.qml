import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Export Photo: full resolution of the crop, with every edit; one photo or every one the filmstrip shows.
Dialog {
    id: dlg
    readonly property var e: Store.s.export || ({})
    readonly property var formats: e.formats || []
    readonly property int fi: Math.max(0, formats.findIndex(f => f.tag === e.format))
    readonly property var fmt: formats[fi] || ({})
    title: Store.tr("Export Photo")
    modal: true
    anchors.centerIn: Overlay.overlay
    width: 460
    padding: 20
    visible: e.open === true
    closePolicy: Popup.CloseOnEscape
    onRejected: Store.cmd("exportClosed")
    function settings(format, quality, all) { Store.cmd("exportSettings", { format: format, quality: quality, all: all }) }
    background: Rectangle { color: Theme.panel; radius: 12; border.color: Theme.line }
    header: Label { text: dlg.title; color: Theme.text; font.pixelSize: 17; font.weight: Font.DemiBold; padding: 20; bottomPadding: 4 }

    ColumnLayout {
        anchors.fill: parent
        spacing: 12
        Caption { text: Store.tr("Full resolution of the crop, with every edit, in sRGB.") }
        GridLayout {
            Layout.fillWidth: true
            columns: 2
            columnSpacing: 12; rowSpacing: 10
            enabled: e.busy !== true
            Label { text: Store.tr("Format"); color: Theme.dim }
            Choice {
                Layout.fillWidth: true
                model: dlg.formats.map(f => f.name)
                currentIndex: dlg.fi
                onActivated: (i) => dlg.settings(dlg.formats[i].tag, e.quality, e.all)
            }
            Label { text: Store.tr("Quality"); color: Theme.dim; visible: dlg.fmt.lossy === true }
            SpinBox {
                visible: dlg.fmt.lossy === true
                from: 1; to: 100; editable: true
                value: e.quality || 90
                onValueModified: dlg.settings(e.format, value, e.all)
            }
            Label { text: Store.tr("HDR headroom"); color: Theme.dim; visible: dlg.fmt.hdr === true }
            Label { text: e.headroom || ""; color: Theme.text; visible: dlg.fmt.hdr === true; wrapMode: Text.Wrap; Layout.fillWidth: true }
            Label { text: Store.tr("Size"); color: Theme.dim }
            Label { text: e.size || ""; color: Theme.text; font.features: { "tnum": 1 } }
            Label { text: Store.tr("File size"); color: Theme.dim }
            Label { text: e.estimate || "…"; color: Theme.text; font.features: { "tnum": 1 } }
        }
        Switch2 {
            visible: (e.roll || 0) > 1
            enabled: e.busy !== true
            title: Store.tr("All photos in the filmstrip")
            subtitle: e.allSubtitle || ""
            checked: e.all === true
            onToggled: (on) => dlg.settings(e.format, e.quality, on)
        }
        ColumnLayout {
            Layout.fillWidth: true
            visible: e.busy === true
            Label { text: e.step || (e.progress > 0.95 ? Store.tr("Saving…") : Store.tr("Developing…")); color: Theme.dim; font.pixelSize: Theme.small }
            ProgressBar { Layout.fillWidth: true; value: e.progress || 0 }
        }
        RowLayout {
            Layout.fillWidth: true
            Item { Layout.fillWidth: true }
            Btn { text: Store.tr("Cancel"); flat: true; visible: e.busy !== true; onClicked: dlg.reject() }
            Btn {
                text: e.busy === true ? Store.tr("Stop") : Store.tr("Export…")
                highlighted: e.busy !== true
                onClicked: Store.cmd("exportGo")
            }
        }
    }
}
