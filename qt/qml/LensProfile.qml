import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// The lensfun profile of the open RAW: found from its EXIF, or chosen; with the focal length and
// aperture to read it at when the photo does not say (a manual lens).
ColumnLayout {
    id: lp
    readonly property var dev: Store.s.develop || ({})
    spacing: 6
    RowLayout {
        Layout.fillWidth: true
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 0
            Label { text: Store.tr("Profile"); color: Theme.text; font.pixelSize: Theme.body }
            Label { text: dev.lensStatus || "—"; color: Theme.dim; font.pixelSize: Theme.small; elide: Text.ElideRight; Layout.fillWidth: true }
        }
        Btn {
            text: Store.tr("Change…")
            flat: true
            tip: Store.tr("Choose the lens profile")
            onClicked: { pop.open(); search.forceActiveFocus(); Store.cmd("lensSearch", { text: search.text }) }
        }
    }
    GridLayout {
        Layout.fillWidth: true
        visible: dev.lensAsked === true
        columns: 2
        Label { text: Store.tr("Focal length (mm)"); color: Theme.dim; font.pixelSize: Theme.small }
        SpinBox {
            id: focal
            from: 0; to: 2000; editable: true
            value: Math.round(dev.lensFocal || 0)
            onValueModified: Store.cmd("lensManual", { focal: value, aperture: aperture.value / 10 })
        }
        Label { text: Store.tr("Aperture (f/)"); color: Theme.dim; font.pixelSize: Theme.small }
        SpinBox {
            id: aperture
            from: 0; to: 640; stepSize: 1; editable: true
            value: Math.round((dev.lensAperture || 0) * 10)
            textFromValue: (v) => (v / 10).toFixed(1)
            valueFromText: (t) => Math.round(parseFloat(t) * 10)
            onValueModified: Store.cmd("lensManual", { focal: focal.value, aperture: value / 10 })
        }
    }
    Popup {
        id: pop
        width: lp.width; height: 320
        y: 40
        padding: 8
        background: Rectangle { color: Theme.panel; border.color: Theme.line; radius: Theme.radius }
        ColumnLayout {
            anchors.fill: parent
            TextField {
                id: search
                Layout.fillWidth: true
                placeholderText: Store.tr("Search lenses")
                onTextChanged: Store.cmd("lensSearch", { text: text })
            }
            ListView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                WheelScroll {}
                // A count, not a fresh array per publish: the rows are kept while the state changes.
                model: 2 + (dev.lensResults || []).length
                delegate: ItemDelegate {
                    required property int index
                    readonly property var lens: index < 2 ? null : (dev.lensResults || [])[index - 2] || {}
                    width: ListView.view.width
                    text: index === 0 ? Store.tr("Automatic (from the photo)") : index === 1 ? Store.tr("No profile") : lens.maker + " " + lens.model
                    onClicked: {
                        if (index === 0) Store.cmd("lensPick", {})
                        else if (index === 1) Store.cmd("lensNone")
                        else Store.cmd("lensPick", { maker: lens.maker, model: lens.model })
                        pop.close()
                    }
                }
                ScrollBar.vertical: ScrollBar {}
            }
        }
    }
}
