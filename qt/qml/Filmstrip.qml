import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// The photos edited together: each through its own grade; stars and colour labels to sort a shoot.
Rectangle {
    id: fs
    readonly property var s: Store.s
    readonly property var roll: s.roll || []
    // The roll positions the filter shows: hidden photos get no card (and leave no gap).
    readonly property var shown: roll.map((c, i) => c.shown !== false ? i : -1).filter(i => i >= 0)
    color: Theme.panel
    implicitHeight: 112

    RowLayout {
        anchors { fill: parent; margins: 8 }
        spacing: 8
        ColumnLayout {
            Layout.fillHeight: true
            Icon { name: "list-add"; tip: Store.tr("Add photos to grade together"); onClicked: Store.cmd("open") }
            Icon {
                name: "view-grid"
                tip: Store.tr("All photos in the filmstrip (G)")
                checkable: true
                checked: s.grid === true
                enabled: fs.roll.length > 1
                onClicked: Store.cmd("grid", { on: !s.grid })
            }
            Item { Layout.fillHeight: true }
        }
        ListView {
            id: list
            Layout.fillWidth: true
            Layout.fillHeight: true
            orientation: ListView.Horizontal
            spacing: 8
            clip: true
            model: fs.shown.length
            boundsBehavior: Flickable.StopAtBounds
            ScrollBar.horizontal: ScrollBar { policy: ScrollBar.AsNeeded }
            delegate: Rectangle {
                id: card
                required property int index
                readonly property int at: fs.shown[index]
                readonly property var it: fs.roll[at] || ({})
                width: height * 1.4
                height: list.height - 8
                radius: Theme.radius
                color: Theme.canvas
                border.color: it.active ? Theme.accent : (hover.hovered ? Theme.line : "transparent")
                border.width: 2
                Image {
                    anchors { fill: parent; margins: 3; bottomMargin: 20 }
                    source: Store.img("roll/" + card.it.id)
                    fillMode: Image.PreserveAspectFit
                    cache: false
                    retainWhileLoading: true
                }
                Rectangle {
                    width: 6; height: 6; radius: 3
                    anchors { left: parent.left; bottom: parent.bottom; margins: 7 }
                    color: Theme.labels[card.it.label] || "transparent"
                }
                Label {
                    anchors { left: parent.left; right: stars.left; bottom: parent.bottom; leftMargin: 18; bottomMargin: 3 }
                    text: card.it.name || ""
                    color: card.it.active ? Theme.text : Theme.dim
                    font.pixelSize: 10; elide: Text.ElideMiddle
                }
                Label {
                    id: stars
                    anchors { right: parent.right; bottom: parent.bottom; rightMargin: 6; bottomMargin: 3 }
                    text: card.it.rating ? "★".repeat(card.it.rating) : ""
                    color: Theme.accent; font.pixelSize: 9
                }
                HoverHandler { id: hover }
                TapHandler { onTapped: Store.cmd("rollSelect", { i: card.at }) }
                Accessible.role: Accessible.Button
                Accessible.name: Store.tr("Edit this photo") + ": " + (card.it.name || "")
                ToolTip.visible: hover.hovered; ToolTip.text: card.it.name || ""; ToolTip.delay: 700
                Icon {
                    visible: hover.hovered
                    anchors { top: parent.top; right: parent.right; margins: 2 }
                    name: "window-close"; size: 12
                    width: 22; height: 22
                    tip: Store.tr("Remove from the filmstrip")
                    onClicked: Store.cmd("rollRemove", { i: card.at })
                }
            }
        }
        Choice {
            Layout.preferredWidth: 150
            Layout.alignment: Qt.AlignTop
            model: Store.c.rollFilters
            currentIndex: s.rollFilter || 0
            Accessible.name: Store.tr("Filter photos")
            ToolTip.visible: hovered; ToolTip.text: Store.tr("Show only some photos: by stars or colour label"); ToolTip.delay: 700
            onActivated: (i) => Store.cmd("rollFilter", { f: i })
        }
    }
}
