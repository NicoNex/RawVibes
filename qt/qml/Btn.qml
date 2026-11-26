import QtQuick
import QtQuick.Controls

// A text button that reads as unavailable when disabled (the Basic style keeps its colours);
// `tip` is its tooltip, as on Icon.
Button {
    property string tip
    opacity: enabled ? 1 : 0.4
    ToolTip.visible: hovered && tip.length > 0
    ToolTip.text: tip
    ToolTip.delay: 600
}
