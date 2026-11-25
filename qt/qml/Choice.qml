import QtQuick
import QtQuick.Controls
import QtQuick.Controls.impl
import RawVibes

// A themed dropdown of strings.
ComboBox {
    id: c
    implicitHeight: 30
    font.pixelSize: Theme.body
    contentItem: Label {
        leftPadding: 10; rightPadding: 24
        text: c.displayText; color: Theme.text; verticalAlignment: Text.AlignVCenter; elide: Text.ElideRight
    }
    indicator: IconImage {
        x: c.width - width - 8; y: (c.height - height) / 2
        source: "qrc:/icons/pan-down-symbolic.svg"; sourceSize: Qt.size(12, 12); color: Theme.dim
    }
    background: Rectangle {
        radius: Theme.radius
        color: c.hovered ? Theme.raised : Theme.canvas
        border.color: c.visualFocus ? Theme.accent : Theme.line
    }
    delegate: ItemDelegate {
        required property var modelData
        required property int index
        width: c.width
        highlighted: c.highlightedIndex === index
        contentItem: Label { text: modelData; color: index === c.currentIndex ? Theme.accent : Theme.text; elide: Text.ElideRight }
        background: Rectangle { color: highlighted ? Theme.raised : Theme.panel }
    }
    popup.background: Rectangle { color: Theme.panel; border.color: Theme.line; radius: Theme.radius }
}
