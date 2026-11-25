import QtQuick
import QtQuick.Controls
import QtQuick.Controls.impl
import QtQuick.Layouts
import RawVibes

// LUT Lab: the combined LUT on the photo.
Rectangle {
    readonly property var s: Store.s
    color: Theme.canvas
    Image {
        anchors { fill: parent; margins: 24 }
        visible: (s.lab || []).length > 0 && s.hasSource
        source: s.labGen ? "image://rv/lab/" + s.labGen : ""
        fillMode: Image.PreserveAspectFit
        cache: false
        retainWhileLoading: true
        Accessible.name: Store.tr("Combined LUT preview")
    }
    ColumnLayout {
        anchors.centerIn: parent
        visible: (s.lab || []).length === 0 || !s.hasSource
        width: Math.min(460, parent.width - 48)
        spacing: 10
        IconImage { Layout.alignment: Qt.AlignHCenter; source: "qrc:/icons/rawvibes-lut-symbolic.svg"; sourceSize: Qt.size(64, 64); color: Theme.faint }
        Label { Layout.alignment: Qt.AlignHCenter; text: "LUT Lab"; color: Theme.text; font.pixelSize: 20; font.weight: Font.DemiBold }
        Label {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            color: Theme.dim
            text: Store.tr("Combine LUTs, grades and profile conversions: chained or mixed, each with its own intensity.\nE.g. a Fujifilm LUT for F-Log becomes a LUT for V-Log or for the Standard profile.")
        }
    }
}
