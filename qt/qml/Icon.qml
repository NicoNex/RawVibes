import QtQuick
import QtQuick.Controls
import RawVibes

// A flat icon button; `name` is a bundled symbolic icon, `checkable` makes it a toggle lit in the accent.
AbstractButton {
    id: b
    property string name
    property string tip
    property int size: 16
    property bool round: true
    implicitWidth: 30; implicitHeight: 30
    hoverEnabled: true
    focusPolicy: Qt.StrongFocus
    Accessible.name: tip
    ToolTip.visible: hovered && tip.length > 0
    ToolTip.text: tip
    ToolTip.delay: 600
    opacity: enabled ? 1 : 0.35
    icon.source: "qrc:/icons/" + name + "-symbolic.svg"
    icon.width: size; icon.height: size
    icon.color: checked ? Theme.accent : (hovered ? Theme.text : Theme.dim)
    contentItem: IconLabel { icon: b.icon; display: AbstractButton.IconOnly }
    background: Rectangle {
        radius: b.round ? height / 2 : Theme.radius
        color: b.down ? Theme.line : (b.hovered || b.checked ? Theme.raised : "transparent")
        border.color: b.visualFocus ? Theme.accent : "transparent"
    }
}
