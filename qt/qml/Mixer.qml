import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Channel mixer: each output channel a mix of the scene's red, green and blue (100% of its own is no change),
// or black & white weighted like a colour filter on film.
ColumnLayout {
    id: m
    readonly property var s: Store.s
    readonly property var names: [Store.tr("Red"), Store.tr("Green"), Store.tr("Blue")]
    readonly property var tracks: [["#1b1b1b", "#e5534b"], ["#1b1b1b", "#57c46a"], ["#1b1b1b", "#5b8def"]]
    property int row: 0
    spacing: 6
    Switch2 {
        title: Store.tr("Black & white")
        checked: s.mixerMono === true
        onToggled: (on) => Store.cmd("mixerMono", { on: on })
    }
    Switch2 {
        title: Store.tr("Preserve luminance"); subtitle: Store.tr("The mix changes colour, not brightness")
        checked: s.mixerFree !== true
        onToggled: (on) => Store.cmd("mixerFree", { on: !on })
    }
    Toggles {
        Layout.fillWidth: true
        visible: s.mixerMono !== true
        model: m.names
        current: m.row
        onPicked: (i) => m.row = i
    }
    Repeater {
        model: 3
        ValueSlider {
            Layout.fillWidth: true
            readonly property bool mono: s.mixerMono === true
            // Output rows start at 100% of their own channel; black & white at the luminance weights.
            readonly property int coeff: mono ? 9 + index : m.row * 3 + index
            readonly property real base: mono ? Math.round((s.luma || [0.2126, 0.7152, 0.0722])[index] * 1000) / 10
                                              : (m.row === index ? 100 : 0)
            label: m.names[index]
            from: -200; to: 200; neutral: base
            track: m.tracks[index]
            value: base + ((s.mixer || [])[coeff] || 0)
            onMoved: (v) => Store.cmd("mixer", { i: coeff, v: v - base })
        }
    }
    Caption {
        text: s.mixerMono === true
            ? Store.tr("Like a colour filter on black & white film: more red brightens skin and darkens skies.")
            : Store.tr("Each output channel is a mix of the scene's red, green and blue; 100% of its own channel is no change.")
    }
}
