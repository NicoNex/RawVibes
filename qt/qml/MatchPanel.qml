import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Reference match: tones and colours of a reference (a film still, a photo you love, the camera's JPEG) onto this photo.
StackLayout {
    readonly property var s: Store.s
    currentIndex: s.matching === "busy" ? 1 : (s.matching === "set" ? 2 : 0)
    ColumnLayout {
        spacing: 10
        Caption { text: Store.tr("Make this photo look like a reference: a film still, a photo you love. Tones and colours are matched; refine the rest with the other tools.") }
        Btn { Layout.alignment: Qt.AlignHCenter; text: Store.tr("Choose Reference…"); onClicked: Store.cmd("matchChoose") }
        Btn {
            Layout.alignment: Qt.AlignHCenter
            visible: s.raw === true
            flat: true
            text: Store.tr("Camera JPEG")
            ToolTip.visible: hovered; ToolTip.text: Store.tr("As in camera: match the JPEG the camera embedded in the RAW"); ToolTip.delay: 600
            onClicked: Store.cmd("matchCamera")
        }
    }
    BusyIndicator { running: s.matching === "busy"; Layout.alignment: Qt.AlignHCenter }
    ColumnLayout {
        spacing: 6
        Image {
            Layout.fillWidth: true
            Layout.preferredHeight: 110
            visible: Store.gen("match") > 0 && (s.matchFile || s.matchName !== "")
            source: Store.img("match")
            fillMode: Image.PreserveAspectCrop
            cache: false
            clip: true
            Accessible.name: Store.tr("Reference photo")
        }
        Label { text: s.matchName || ""; color: Theme.dim; font.pixelSize: Theme.small; elide: Text.ElideMiddle; Layout.fillWidth: true }
        ValueSlider {
            Layout.fillWidth: true
            label: Store.tr("Strength"); from: 0; to: 100; neutral: 100
            value: s.matchAmount || 0
            onMoved: (v) => Store.cmd("matchAmount", { v: v })
        }
        RowLayout {
            Layout.alignment: Qt.AlignRight
            Btn { flat: true; text: Store.tr("Change…"); onClicked: Store.cmd("matchChoose") }
            Btn { flat: true; text: Store.tr("Remove"); onClicked: Store.cmd("matchClear") }
        }
    }
}
