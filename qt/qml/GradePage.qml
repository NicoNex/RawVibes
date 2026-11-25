import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// The Grade tab, a grading workflow top to bottom: the look to grade under, balance, primaries,
// secondaries, the look's finishing touches. All of it goes into the exported LUT.
ColumnLayout {
    id: page
    readonly property var s: Store.s
    readonly property var p: s.params || ({})
    spacing: 4

    component Param: ValueSlider {
        property string key
        Layout.fillWidth: true
        value: p[key] || 0
        onMoved: (v) => Store.cmd("set", { key: key, v: v })
    }

    Label { text: Store.tr("In the exported LUT"); color: Theme.faint; font.pixelSize: Theme.small; Layout.leftMargin: 6 }
    Label {
        Layout.fillWidth: true
        Layout.leftMargin: 6
        textFormat: Text.StyledText
        wrapMode: Text.Wrap
        color: Theme.dim
        font.pixelSize: Theme.small + 1
        linkColor: Theme.accent
        text: Store.tr("Exported LUT input: <b>{}</b>. <a href=\"photo\">Change in Photo</a>").replace("{}", s.profileName || "")
        onLinkActivated: Store.cmd("tab", { tab: "photo" })
    }

    PhaseLabel { text: Store.tr("Look") }
    Section {
        title: Store.tr("LUTs")
        eye: "looks"
        Looks { Layout.fillWidth: true }
    }

    PhaseLabel { text: Store.tr("Balance") }
    Section {
        title: Store.tr("Reference match")
        eye: "g:Match"
        open: false
        MatchPanel { Layout.fillWidth: true }
    }
    Section {
        title: Store.tr("Warmth")
        eye: "g:WhiteBalance"
        Param { label: Store.tr("Temperature"); key: "temperature"; track: ["#4f86e8", "#e8e8e8", "#f2b84b"] }
        Param { label: Store.tr("Tint"); key: "tint"; track: ["#4cbf5a", "#e8e8e8", "#d85bd0"] }
    }

    PhaseLabel { text: Store.tr("Primaries") }
    Section {
        title: Store.tr("Tone")
        eye: "g:Tone"
        Param { label: Store.tr("Exposure"); key: "exposure"; from: -3; to: 3; decimals: 2; unit: " EV" }
        Param { label: Store.tr("Contrast"); key: "contrast" }
        Param { label: Store.tr("Highlights"); key: "highlights" }
        Param { label: Store.tr("Shadows"); key: "shadows" }
        Param { label: Store.tr("Whites"); key: "whites" }
        Param { label: Store.tr("Blacks"); key: "blacks" }
    }
    Section {
        title: Store.tr("Curves")
        eye: "g:Curves"
        CurveEditor {
            Layout.fillWidth: true
            first: 0; last: 3
            names: ["Luma", Store.tr("Red"), Store.tr("Green"), Store.tr("Blue")]
            hint: Store.tr("Click to add a point, double-click or right-click to remove it")
        }
    }
    Section {
        title: Store.tr("Primary colour wheels")
        eye: "g:Wheels"
        GridLayout {
            Layout.fillWidth: true
            columns: 2
            columnSpacing: 10
            rowSpacing: 10
            Repeater {
                model: ["Lift", "Gamma", "Gain", "Offset"]
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 4
                    readonly property var w: (s.wheels || [])[index] || { x: 0, y: 0, luma: 0 }
                    Label { text: modelData; color: Theme.dim; font.pixelSize: Theme.small; font.weight: Font.DemiBold; Layout.alignment: Qt.AlignHCenter }
                    ColorWheel {
                        Layout.alignment: Qt.AlignHCenter
                        Layout.preferredWidth: Math.min(150, page.width / 2 - 30); Layout.preferredHeight: Layout.preferredWidth
                        name: modelData
                        px: parent.w.x; py: parent.w.y
                        onMoved: (x, y) => Store.cmd("wheel", { i: index, x: x, y: y })
                    }
                    ValueSlider {
                        Layout.fillWidth: true
                        label: "Luma"
                        value: parent.w.luma
                        onMoved: (v) => Store.cmd("wheelLuma", { i: index, v: v })
                    }
                }
            }
        }
        Caption { text: Store.tr("Drag a wheel's centre towards a colour to tint that range, away from it to remove it; Luma brightens or darkens it. Lift is the shadows, Gamma the midtones, Gain the highlights, Offset everything. Double-click a wheel to reset it.") }
    }
    Section {
        title: Store.tr("Colour")
        eye: "g:Color"
        Param { label: Store.tr("Saturation"); key: "saturation" }
        Param { label: Store.tr("Vibrance"); key: "vibrance" }
        Param { label: Store.tr("Shadow saturation"); key: "sat_shadows" }
        Param { label: Store.tr("Highlight saturation"); key: "sat_highlights" }
    }

    PhaseLabel { text: Store.tr("Secondaries") }
    Section {
        title: Store.tr("Selective colour")
        eye: "g:Selective"
        Row {
            Layout.alignment: Qt.AlignHCenter
            spacing: 6
            Repeater {
                model: 8
                AbstractButton {
                    readonly property var modelData: (s.bands || [])[index] || { swatch: "transparent" }
                    width: 22; height: 22
                    checked: index === s.band
                    Accessible.name: (s.bandNames || [])[index] || ""
                    ToolTip.visible: hovered; ToolTip.text: Accessible.name; ToolTip.delay: 500
                    onClicked: Store.cmd("band", { i: index })
                    background: Rectangle {
                        radius: 11; color: modelData.swatch
                        border.color: parent.checked ? Theme.text : (parent.visualFocus ? Theme.accent : "transparent")
                        border.width: 2
                    }
                }
            }
        }
        Label {
            Layout.alignment: Qt.AlignHCenter
            text: (s.bandNames || [])[s.band || 0] || ""
            color: Theme.dim; font.pixelSize: Theme.small; font.weight: Font.DemiBold
        }
        Repeater {
            model: [[Store.tr("Hue"), "hue"], [Store.tr("Saturation"), "sat"], [Store.tr("Luminance"), "lum"]]
            ValueSlider {
                Layout.fillWidth: true
                label: modelData[0]
                track: s.bands ? s.bands[s.band || 0][modelData[1]] : []
                value: (s.hue || [0, 0, 0])[index]
                onMoved: (v) => Store.cmd("hue", { k: index, v: v })
            }
        }
    }
    Section {
        title: Store.tr("Hue curves")
        eye: "g:HueCurves"
        open: false
        CurveEditor {
            Layout.fillWidth: true
            first: 4; last: 6
            names: [Store.tr("Hue vs Hue"), Store.tr("Hue vs Sat"), Store.tr("Lum vs Sat")]
            hint: Store.tr("The middle line is no change. Pull a hue up or down to shift it, saturate or desaturate it; Lum vs Sat works by brightness.")
        }
    }

    PhaseLabel { text: Store.tr("Finishing") }
    Section {
        title: Store.tr("Film colour")
        eye: "g:Film"
        Param { label: Store.tr("Subtractive saturation"); key: "film_sat"; track: ["#8a8580", "#b34a3c", "#5a1f2a"] }
        Param { label: Store.tr("Colour density"); key: "film_density"; from: 0; track: ["#d9774f", "#7a2c22"] }
        Caption { text: Store.tr("Film makes colour by adding dye, so richer colours are also deeper. Subtractive saturation saturates the way dye does; density deepens saturated colours. Greys are untouched.") }
        Param { label: Store.tr("Soft light"); key: "soft_light"; from: 0 }
        Caption { text: Store.tr("Soft light deepens shadows and lifts highlights, as RawTherapee's; 30 is its default.") }
    }
    Section {
        title: Store.tr("Channel mixer")
        eye: "g:Mixer"
        open: false
        Mixer { Layout.fillWidth: true }
    }
}
