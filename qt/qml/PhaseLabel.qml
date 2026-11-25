import QtQuick
import QtQuick.Layouts
import RawVibes

// A step of the workflow over the sections that follow ("Light and colour", "Geometry"…).
RowLayout {
    property alias text: t.text
    Layout.fillWidth: true
    Layout.topMargin: 10
    spacing: 8
    Text { id: t; color: Theme.faint; font.pixelSize: Theme.small; font.weight: Font.DemiBold }
    Rectangle { Layout.fillWidth: true; height: 1; color: Theme.line }
}
