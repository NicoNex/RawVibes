import QtQuick
import QtQuick.Controls
import RawVibes

// Every filmstrip photo at once, each through its own grade: pick one to edit it.
Rectangle {
    readonly property var roll: Store.s.roll || []
    color: Theme.canvas
    GridView {
        id: g
        anchors { fill: parent; margins: 16 }
        readonly property int cols: Math.max(2, Math.ceil(Math.sqrt(roll.length * width / Math.max(1, height))))
        cellWidth: width / cols
        cellHeight: cellWidth * 0.72
        model: roll.length
        clip: true
        delegate: Item {
            required property int index
            readonly property var it: roll[index] || ({})
            width: g.cellWidth; height: g.cellHeight
            visible: it.shown !== false
            Rectangle {
                anchors { fill: parent; margins: 6 }
                radius: Theme.radius
                color: Theme.panel
                border.color: it.active ? Theme.accent : (h.hovered ? Theme.line : "transparent")
                border.width: 2
                Image {
                    anchors { fill: parent; margins: 4; bottomMargin: 22 }
                    source: it.frame ? "image://rv/roll/" + index + "/" + it.frame : ""
                    fillMode: Image.PreserveAspectFit
                    cache: false
                    retainWhileLoading: true
                }
                Label {
                    anchors { left: parent.left; right: parent.right; bottom: parent.bottom; margins: 5 }
                    text: (it.name || "") + (it.rating ? "  " + "★".repeat(it.rating) : "")
                    color: Theme.dim; font.pixelSize: Theme.small; elide: Text.ElideMiddle
                }
                HoverHandler { id: h }
                TapHandler { onTapped: { Store.cmd("rollSelect", { i: index }); Store.cmd("grid", { on: false }) } }
            }
        }
    }
}
