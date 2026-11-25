import QtQuick
import QtQuick.Controls
import RawVibes

// The open photo's stars (a star clicked twice clears them) and colour label.
Row {
    readonly property var s: Store.s
    readonly property var colours: ({ None: Theme.line, Red: "#e5534b", Yellow: "#e0c040", Green: "#57c46a", Blue: "#5b8def", Purple: "#b06ce0" })
    spacing: 0
    Repeater {
        model: 5
        AbstractButton {
            width: 20; height: 30
            Accessible.name: Store.tr("Stars") + " " + (index + 1)
            onClicked: Store.cmd("rate", { n: index + 1, toggle: true })
            contentItem: Text {
                text: index < (s.rating || 0) ? "★" : "☆"
                color: index < (s.rating || 0) ? Theme.accent : (parent.hovered ? Theme.dim : Theme.faint)
                font.pixelSize: 15
                horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter
            }
            background: null
        }
    }
    AbstractButton {
        id: labelBtn
        width: 30; height: 30
        Accessible.name: Store.tr("Colour label: {}").replace("{}", s.labelName || "")
        ToolTip.visible: hovered; ToolTip.text: Accessible.name; ToolTip.delay: 600
        onClicked: labelMenu.popup(labelBtn, 0, labelBtn.height)
        contentItem: Item {
            Rectangle { anchors.centerIn: parent; width: 12; height: 12; radius: 6; color: colours[s.label] || Theme.line; border.color: Theme.dim; border.width: s.label === "None" ? 1 : 0 }
        }
        background: Rectangle { radius: 15; color: labelBtn.hovered ? Theme.raised : "transparent" }
        Menu {
            id: labelMenu
            Repeater {
                model: Store.c.labels
                MenuItem {
                    text: modelData.name
                    checkable: true
                    checked: s.label === modelData.key
                    onTriggered: Store.cmd("mark", { label: modelData.key })
                }
            }
        }
    }
}
