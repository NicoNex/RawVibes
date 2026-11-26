import QtQuick
import QtQuick.Controls
import RawVibes

// Every filmstrip photo at once, each through its own grade: pick one to edit it.
Rectangle {
    readonly property var roll: Store.s.roll || []
    // The roll positions the filter shows: hidden photos leave no empty cell.
    readonly property var shown: roll.map((c, i) => c.shown !== false ? i : -1).filter(i => i >= 0)
    color: Theme.canvas
    GridView {
        id: g
        anchors { fill: parent; margins: 16 }
        readonly property int cols: Math.max(2, Math.ceil(Math.sqrt(shown.length * width / Math.max(1, height))))
        cellWidth: width / cols
        cellHeight: cellWidth * 0.72
        model: shown.length
        clip: true
        delegate: Item {
            required property int index
            readonly property int at: shown[index]
            readonly property var it: roll[at] || ({})
            width: g.cellWidth; height: g.cellHeight
            Rectangle {
                anchors { fill: parent; margins: 6 }
                radius: Theme.radius
                color: Theme.panel
                border.color: it.active ? Theme.accent : (h.hovered ? Theme.line : "transparent")
                border.width: 2
                Image {
                    anchors { fill: parent; margins: 4; bottomMargin: 22 }
                    source: Store.img("roll/" + it.id)
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
                TapHandler { onTapped: { Store.cmd("rollSelect", { i: at }); Store.cmd("grid", { on: false }) } }
            }
        }
    }
}
