import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// The LUT library on this photo: each card is the photo through the grade, the looks and that LUT.
// Resting on a card previews it; a click selects it, a double-click (or Enter) adds it to the looks.
ColumnLayout {
    id: br
    readonly property var s: Store.s
    readonly property var cards: s.gallery || []
    spacing: 8

    RowLayout {
        Layout.fillWidth: true
        Icon { name: "go-previous"; tip: Store.tr("Back to the grade (B)"); onClicked: Store.cmd("browse", { on: false }) }
        Heading { text: Store.tr("LUTs"); Layout.fillWidth: true }
        Icon { name: "folder-open"; tip: Store.tr("Choose LUT folder"); onClicked: Store.cmd("chooseFolder") }
        Icon { name: "view-refresh"; tip: Store.tr("Reload LUTs (F5)"); onClicked: Store.cmd("reload") }
        Icon {
            name: "applications-science"
            tip: Store.tr("Open the selected LUT in LUT Lab (to convert or combine it)")
            enabled: s.selected !== null && s.selected !== undefined
            onClicked: Store.cmd("openInLab")
        }
    }
    Label {
        Layout.fillWidth: true
        text: s.lutFolder ? s.lutFolder.split(/[\\/]/).pop() : Store.tr("No folder")
        color: Theme.dim; font.pixelSize: Theme.small; elide: Text.ElideMiddle
    }
    TextField {
        id: search
        Layout.fillWidth: true
        placeholderText: Store.tr("Search LUTs")
        onTextChanged: Store.cmd("search", { text: text })
        Component.onCompleted: forceActiveFocus()
    }
    Choice {
        Layout.fillWidth: true
        visible: (s.groups || []).length > 2
        // Rebuilt only when the folders change, not on every publish (a hover publishes).
        readonly property string groupsKey: (s.groups || []).join("\n")
        model: groupsKey.split("\n")
        currentIndex: s.groupIndex || 0
        Accessible.name: Store.tr("Subfolder")
        onActivated: (i) => Store.cmd("group", { i: i })
    }

    ColumnLayout {
        visible: br.cards.length === 0
        Layout.fillWidth: true
        Layout.topMargin: 24
        Label { Layout.alignment: Qt.AlignHCenter; text: Store.tr("No LUTs"); color: Theme.text; font.pixelSize: 16; font.weight: Font.DemiBold }
        Label { Layout.alignment: Qt.AlignHCenter; text: Store.tr("Choose a folder with .cube files"); color: Theme.dim }
        Btn { Layout.alignment: Qt.AlignHCenter; text: Store.tr("Choose LUT folder"); onClicked: Store.cmd("chooseFolder") }
    }

    GridView {
        id: grid
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.minimumHeight: 200
        clip: true
        visible: br.cards.length > 0
        readonly property int cols: Math.max(2, Math.floor(width / 120))
        cellWidth: Math.floor(width / cols)
        cellHeight: cellWidth * 0.62 + 26
        // By count, so cards are rebuilt only when the filter changes what shows.
        readonly property var shown: br.cards.map((c, i) => c.shown ? i : -1).filter(i => i >= 0)
        model: shown.length
        keyNavigationEnabled: true
        // Nothing selected until the user picks a card.
        currentIndex: -1
        focus: true
        Keys.onReturnPressed: if (currentItem) Store.cmd("commit", { i: currentItem.card.i })
        // The selection follows the keyboard, never a model reset (which moves currentIndex by itself).
        Keys.onReleased: (e) => {
            if ([Qt.Key_Left, Qt.Key_Right, Qt.Key_Up, Qt.Key_Down].includes(e.key) && currentItem)
                Store.cmd("selectLut", { i: currentItem.card.i })
        }
        ScrollBar.vertical: ScrollBar {}
        delegate: Item {
            id: cell
            required property int index
            readonly property var card: Object.assign({ i: grid.shown[index] }, br.cards[grid.shown[index]] || {})
            width: grid.cellWidth; height: grid.cellHeight
            readonly property bool selected: s.selected === card.i
            readonly property bool previewed: s.candidate === card.i
            Rectangle {
                anchors { fill: parent; margins: 4 }
                radius: Theme.radius
                color: hover.hovered ? Theme.raised : "transparent"
                border.color: cell.selected ? Theme.accent : (cell.previewed ? Theme.dim : "transparent")
                border.width: 2
                ColumnLayout {
                    anchors { fill: parent; margins: 4 }
                    spacing: 4
                    Image {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        source: Store.img("thumb/" + card.i)
                        fillMode: Image.PreserveAspectCrop
                        cache: false
                        clip: true
                        retainWhileLoading: true
                        Rectangle { anchors.fill: parent; color: Theme.canvas; visible: !Store.f["thumb/" + card.i]; z: -1 }
                        Label {
                            anchors { top: parent.top; right: parent.right; margins: 4 }
                            text: card.badge
                            color: Theme.text; font.pixelSize: 9; font.weight: Font.Bold
                            padding: 2; leftPadding: 4; rightPadding: 4
                            background: Rectangle { color: "#b0000000"; radius: 3 }
                        }
                    }
                    Label { Layout.fillWidth: true; text: card.title; color: Theme.text; font.pixelSize: Theme.small; elide: Text.ElideRight }
                }
                HoverHandler { id: hover; onHoveredChanged: if (hovered) Store.cmd("hover", { i: card.i }) }
                TapHandler {
                    onTapped: { grid.currentIndex = cell.index; Store.cmd("selectLut", { i: card.i }) }
                    onDoubleTapped: Store.cmd("commit", { i: card.i })
                }
                ToolTip.visible: hover.hovered
                ToolTip.text: card.tip
                ToolTip.delay: 900
            }
        }
        HoverHandler { onHoveredChanged: if (!hovered) Store.cmd("hover", {}) }
    }

    Item { Layout.fillHeight: true; visible: !grid.visible }

    // The applied looks under the grid, so a LUT added by mistake goes without leaving the browser.
    ScrollView {
        Layout.fillWidth: true
        Layout.preferredHeight: Math.min(320, docked.implicitHeight)
        visible: (s.looks || []).length > 0
        clip: true
        Looks { id: docked; width: parent.width; docked: true }
    }
}
