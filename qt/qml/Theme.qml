pragma Singleton
import QtQuick

// Darkroom neutrals: greys without a cast, so they never tint how a photo's colour is judged.
// One accent, a safelight amber, marks what is active.
QtObject {
    readonly property color canvas: "#1c1c1c"
    readonly property color panel: "#242424"
    readonly property color raised: "#2c2c2c"
    readonly property color line: "#383838"
    readonly property color text: "#e8e8e8"
    readonly property color dim: "#9c9c9c"
    readonly property color faint: "#6a6a6a"
    readonly property color accent: "#e0a040"
    readonly property color accentInk: "#1c1c1c"
    readonly property color danger: "#e5534b"

    readonly property int radius: 6
    readonly property int gap: 8
    readonly property int pad: 12
    readonly property int small: 11
    readonly property int body: 13
    readonly property int heading: 14
}
