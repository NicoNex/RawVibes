import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// The Photo tab: the photo's signal, then its development (RAWs only), in the order they are usually done:
// light and colour, geometry, detail judged at 100%, then the vignette effect.
ColumnLayout {
    id: page
    readonly property var s: Store.s
    readonly property var dev: s.develop || ({ detail: {} })
    // The camera's as-shot white balance: Kelvin, tint.
    readonly property var ws: dev.asShot || [5500, 0]
    readonly property var kelvins: Store.c.kelvinRange || [2000, 12000]
    readonly property bool raw: s.raw === true
    spacing: 4

    component Detail: ValueSlider {
        property string key
        value: dev.detail[key] || 0
        neutral: Store.c.detailDefaults && Store.c.detailDefaults[key] !== undefined ? Store.c.detailDefaults[key] : 0
        onMoved: (v) => Store.cmd("detail", { key: key, v: v })
    }
    component Dev: Section {
        enabled: page.raw
        ToolTip.visible: !page.raw && devHover.hovered
        ToolTip.text: Store.tr("RAW development: open a RAW file to use it")
        HoverHandler { id: devHover }
    }

    Section {
        title: Store.tr("Input profile")
        Choice {
            Layout.fillWidth: true
            model: Store.c.profileNames
            currentIndex: s.profileIndex || 0
            Accessible.name: Store.tr("Input profile")
            onActivated: (i) => Store.cmd("profile", { profile: Store.c.profiles[i].key })
        }
        Caption { text: s.profileHint || "" }
    }
    Section {
        title: Store.tr("HDR")
        Switch2 {
            title: Store.tr("HDR preview")
            subtitle: Store.tr("On an HDR screen, the brighter highlights the HDR exports carry")
            checked: s.hdrPreview === true
            onToggled: (on) => Store.cmd("hdrPreview", { on: on })
        }
        ValueSlider {
            Layout.fillWidth: true
            label: Store.tr("Headroom"); from: 1; to: 3; decimals: 1; neutral: 2
            value: s.headroom || 2
            onMoved: (v) => Store.cmd("headroom", { v: v })
        }
        Caption { text: Store.tr("Stops above SDR white the highlights of a RAW or log photo reach in the HDR exports (JPEG HDR, JPEG XL, AVIF, PNG HDR). The SDR photo, the grade and the LUT stay as they are.") }
    }

    Label { text: Store.tr("Not in the exported LUT"); color: Theme.faint; font.pixelSize: Theme.small; Layout.topMargin: 6; Layout.leftMargin: 6 }

    PhaseLabel { text: Store.tr("Light and colour") }
    Dev {
        title: Store.tr("White balance")
        eye: "d:WhiteBalance"
        header: Icon { name: "camera-photo"; tip: Store.tr("As in camera: white balance as shot"); onClicked: Store.cmd("asShot") }
        RowLayout {
            Layout.fillWidth: true
            Choice {
                Layout.fillWidth: true
                model: Store.c.wbPresets
                currentIndex: dev.wbRow || 0
                Accessible.name: Store.tr("White balance preset")
                onActivated: (i) => Store.cmd("wbPreset", { i: i })
            }
            Icon {
                name: "color-select"
                tip: Store.tr("Pick a neutral grey on the photo (Escape cancels)")
                checkable: true
                checked: s.picker === true
                onClicked: Store.cmd("pick", { on: !s.picker })
            }
        }
        ValueSlider {
            Layout.fillWidth: true
            label: Store.tr("Temperature")
            from: kelvins[0]; to: kelvins[1]
            neutral: ws[0]
            unit: " K"
            track: Theme.temperatureTrack
            value: dev.kelvin || 5500
            onMoved: (v) => Store.cmd("kelvin", { v: v })
        }
        ValueSlider {
            Layout.fillWidth: true
            label: Store.tr("Tint")
            track: Theme.tintTrack
            neutral: ws[1]
            value: dev.tint || 0
            onMoved: (v) => Store.cmd("tint", { v: v })
        }
    }
    Dev {
        title: Store.tr("Exposure")
        eye: "d:Exposure"
        header: Icon { name: "camera-photo"; tip: Store.tr("As in camera: the exposure the camera asks for"); onClicked: Store.cmd("cameraExposure") }
        ValueSlider {
            Layout.fillWidth: true
            label: Store.tr("Exposure"); from: -3; to: 3; decimals: 2; unit: " EV"
            value: dev.exposure || 0
            onMoved: (v) => Store.cmd("rawExposure", { v: v })
        }
    }
    Dev {
        title: Store.tr("Shadows and highlights")
        eye: "d:Tones"
        Detail { Layout.fillWidth: true; label: Store.tr("Highlights"); key: "highlights" }
        Detail { Layout.fillWidth: true; label: Store.tr("Shadows"); key: "shadows" }
        Caption { text: Store.tr("Brightens or darkens whole areas and stops at their edges: no halos, detail kept.") }
    }
    Dev {
        title: Store.tr("Presence")
        eye: "d:Presence"
        Detail { Layout.fillWidth: true; label: Store.tr("Dehaze"); key: "dehaze" }
        Detail { Layout.fillWidth: true; label: Store.tr("Local contrast"); key: "local_contrast"; from: 0 }
    }

    PhaseLabel { text: Store.tr("Geometry") }
    Dev {
        title: Store.tr("Lens")
        eye: "d:Lens"
        LensProfile { Layout.fillWidth: true }
        ColumnLayout {
            Layout.fillWidth: true
            enabled: dev.lensProfile === true
            Detail { Layout.fillWidth: true; label: Store.tr("Distortion"); key: "lens_dist_amount"; from: 0 }
            Detail { Layout.fillWidth: true; label: Store.tr("Vignetting"); key: "lens_vig_amount"; from: 0 }
            Switch2 {
                title: Store.tr("Chromatic aberration"); subtitle: Store.tr("The profile's lateral CA correction")
                checked: dev.lensTca === true
                onToggled: (on) => Store.cmd("lensTca", { on: on })
            }
        }
        Label { text: Store.tr("Manual corrections"); color: Theme.dim; font.pixelSize: Theme.small; font.weight: Font.DemiBold; Layout.topMargin: 8 }
        Detail { Layout.fillWidth: true; label: Store.tr("Distortion"); key: "lens_distortion" }
        Detail { Layout.fillWidth: true; label: Store.tr("Vignetting"); key: "lens_vignette" }
        Detail { Layout.fillWidth: true; label: Store.tr("Midpoint"); key: "lens_vignette_mid" }
        Detail { Layout.fillWidth: true; label: Store.tr("Red/cyan fringe"); key: "lens_ca_red" }
        Detail { Layout.fillWidth: true; label: Store.tr("Blue/yellow fringe"); key: "lens_ca_blue" }
        Detail { Layout.fillWidth: true; label: Store.tr("Purple fringe"); key: "defringe"; from: 0 }
        Detail { Layout.fillWidth: true; label: Store.tr("Green fringe"); key: "defringe_green"; from: 0 }
        Switch2 {
            title: Store.tr("Automatic chromatic aberration"); subtitle: Store.tr("Removes red/cyan fringes")
            checked: dev.caCorrect === true
            onToggled: (on) => Store.cmd("caCorrect", { on: on })
        }
    }
    Section {
        title: Store.tr("Crop")
        eye: "crop"
        enabled: s.hasPhoto === true && !s.chart
        RowLayout {
            Layout.fillWidth: true
            Choice {
                Layout.fillWidth: true
                model: Store.c.ratios
                currentIndex: s.crop ? s.crop.ratio : 0
                Accessible.name: Store.tr("Aspect ratio")
                onActivated: (i) => Store.cmd("cropRatio", { i: i })
            }
            Icon {
                name: "object-rotate-right"; tip: Store.tr("Swap orientation")
                enabled: s.crop && s.crop.swappable
                onClicked: Store.cmd("cropSwap")
            }
        }
        RowLayout {
            Layout.fillWidth: true
            ValueSlider {
                Layout.fillWidth: true
                label: Store.tr("Straighten"); from: -45; to: 45; decimals: 1; unit: "°"
                value: s.crop ? s.crop.angle : 0
                onMoved: (v) => Store.cmd("cropAngle", { v: v })
            }
            Icon {
                name: "object-flip-vertical"
                tip: Store.tr("Straighten along a line: drag over a horizon or an edge")
                checkable: true
                checked: !!(s.crop && s.crop.levelling)
                onClicked: Store.cmd("level", { on: !(s.crop && s.crop.levelling) })
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Btn {
                Layout.fillWidth: true
                text: Store.tr("Edit crop")
                checkable: true
                checked: s.crop && s.crop.editing
                ToolTip.visible: hovered; ToolTip.text: Store.tr("Edit crop (R)"); ToolTip.delay: 600
                onClicked: Store.cmd("cropEdit", { on: !(s.crop && s.crop.editing) })
            }
            Btn { text: Store.tr("Reset"); flat: true; onClicked: Store.cmd("cropReset") }
        }
    }

    PhaseLabel { text: Store.tr("Detail") }
    Dev {
        title: Store.tr("Noise reduction")
        eye: "d:Noise"
        Toggles {
            Layout.fillWidth: true
            model: [Store.tr("Classic"), Store.tr("AI")]
            current: dev.nrAi ? 1 : 0
            Accessible.name: Store.tr("Noise reduction method")
            onPicked: (i) => Store.cmd("nrAi", { on: i === 1 })
        }
        RowLayout {
            Layout.fillWidth: true
            visible: dev.aiBusy === true && dev.nrAi === true
            Label { text: Store.tr("Denoising…"); color: Theme.dim; font.pixelSize: Theme.small }
            ProgressBar { Layout.fillWidth: true; value: dev.aiFraction || 0 }
            Icon { name: "process-stop"; tip: Store.tr("Stop and use classic noise reduction"); onClicked: Store.cmd("nrAi", { on: false }) }
        }
        Caption { visible: !!dev.aiMsg; text: dev.aiMsg || ""; color: Theme.accent }
        Detail { Layout.fillWidth: true; visible: dev.nrAi === true; label: Store.tr("Strength"); key: "nr_ai_strength"; from: 0 }
        Detail { Layout.fillWidth: true; visible: dev.nrAi !== true; label: Store.tr("Luminance"); key: "nr_luma"; from: 0 }
        Detail {
            Layout.fillWidth: true; visible: dev.nrAi !== true; label: Store.tr("Detail"); key: "nr_detail"; from: 0
            enabled: (dev.detail.nr_luma || 0) > 0
        }
        Detail { Layout.fillWidth: true; label: Store.tr("Chrominance"); key: "nr_chroma"; from: 0 }
    }
    Dev {
        title: Store.tr("Detail")
        eye: "d:Detail"
        Detail { Layout.fillWidth: true; label: Store.tr("Capture sharpening"); key: "sharpen"; from: 0 }
        Detail { Layout.fillWidth: true; label: Store.tr("Capture radius"); key: "sharpen_radius"; from: 0.5; to: 2; decimals: 1 }
        Detail { Layout.fillWidth: true; label: Store.tr("Sharpening"); key: "usm"; from: 0 }
        Detail { Layout.fillWidth: true; label: Store.tr("Sharpening radius"); key: "usm_radius"; from: 0.5; to: 3; decimals: 1 }
        Detail { Layout.fillWidth: true; label: Store.tr("Sharpening threshold"); key: "usm_threshold"; from: 0 }
    }

    PhaseLabel { text: Store.tr("Effects") }
    Section {
        title: Store.tr("Vignette")
        eye: "g:Vignette"
        open: false
        Repeater {
            model: [["vignette", Store.tr("Amount")], ["vignette_mid", Store.tr("Midpoint")],
                    ["vignette_round", Store.tr("Roundness")], ["vignette_feather", Store.tr("Feather")]]
            ValueSlider {
                Layout.fillWidth: true
                label: modelData[1]
                value: s.params ? s.params[modelData[0]] : 0
                onMoved: (v) => Store.cmd("set", { key: modelData[0], v: v })
            }
        }
    }
}
