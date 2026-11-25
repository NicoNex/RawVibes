import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtCore
import RawVibes

ApplicationWindow {
    id: win
    readonly property var s: Store.s
    width: 1480; height: 920
    minimumWidth: 960; minimumHeight: 600
    visible: true
    title: (s.hasPhoto ? s.title + " — " : "") + "RawVibes"
    color: Theme.canvas
    palette {
        window: Theme.panel; windowText: Theme.text; base: Theme.canvas; text: Theme.text
        button: Theme.raised; buttonText: Theme.text; highlight: Theme.accent; highlightedText: Theme.accentInk
        // Basic style: highlighted and checked buttons draw in `dark` with `brightText`: the accent.
        brightText: Theme.accentInk
        placeholderText: Theme.faint; toolTipBase: Theme.raised; toolTipText: Theme.text; mid: Theme.line; dark: Theme.accent; light: Theme.line
    }
    font.pixelSize: Theme.body

    Settings {
        id: layout
        category: "window"
        property real sidebar: 380
        property bool stripHidden: false
        property bool scopesPinned: true
    }
    onClosing: Store.cmd("close")

    // Debugging aid: `--shot=<file.png>` saves the window once it settles, then quits.
    readonly property string shot: (Qt.application.arguments.find(a => a.startsWith("--shot=")) || "").substring(7)
    Component.onCompleted: {
        const tab = (Qt.application.arguments.find(a => a.startsWith("--tab=")) || "").substring(6)
        if (tab) Store.cmd("tab", { tab: tab })
    }
    Timer {
        running: win.shot !== ""
        interval: 6000
        onTriggered: root.grabToImage(r => { r.saveToFile(win.shot); Store.cmd("close"); Qt.quit() })
    }
    Connections { target: Store; function onPrefsRequested() { prefs.open() } }

    ColumnLayout {
    id: root
    anchors.fill: parent
    spacing: 0
    Rectangle {
        Layout.fillWidth: true
        height: 48
        color: Theme.panel
        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.line }
        RowLayout {
            anchors { fill: parent; leftMargin: 8; rightMargin: 8 }
            spacing: 2
            Icon { name: "document-open"; tip: Store.tr("Open Photo (Ctrl+O)"); onClicked: Store.cmd("open") }
            Item { width: 6 }
            Icon { name: "edit-undo"; tip: Store.tr("Undo (Ctrl+Z)"); enabled: s.undo === true; onClicked: Store.cmd("undo") }
            Icon { name: "edit-redo"; tip: Store.tr("Redo (Ctrl+Shift+Z)"); enabled: s.redo === true; onClicked: Store.cmd("redo") }
            Icon { name: "edit-copy"; tip: Store.tr("Copy edits (Ctrl+Shift+C)"); enabled: s.hasPhoto === true; onClicked: Store.cmd("copyEdits") }
            Icon { name: "edit-paste"; tip: Store.tr("Paste edits (Ctrl+Shift+V)"); enabled: s.clipboard === true && s.hasPhoto === true; onClicked: Store.cmd("pasteEdits") }
            Icon {
                name: "media-playlist-repeat"
                tip: Store.tr("Paste edits to every photo in the filmstrip, crops kept")
                enabled: s.clipboard === true && (s.roll || []).length > 1
                onClicked: Store.cmd("pasteEditsAll")
            }
            Item { Layout.fillWidth: true }
            ColumnLayout {
                spacing: 0
                Label { Layout.alignment: Qt.AlignHCenter; text: s.hasPhoto ? s.title : "RawVibes"; color: Theme.text; font.weight: Font.DemiBold; elide: Text.ElideMiddle; Layout.maximumWidth: 360 }
                Label { Layout.alignment: Qt.AlignHCenter; visible: (s.photoInfo || "") !== ""; text: s.photoInfo || ""; color: Theme.dim; font.pixelSize: Theme.small; elide: Text.ElideRight; Layout.maximumWidth: 360 }
            }
            Marks { visible: s.hasPhoto === true }
            Item { Layout.fillWidth: true }
            Icon {
                name: "magic-wand"
                tip: Store.tr("As in camera: white balance and exposure as shot")
                enabled: s.raw === true
                onClicked: Store.cmd("asInCamera")
            }
            Icon {
                name: "image-x-generic"
                tip: Store.tr("Test chart: shows what the LUT does to neutrals, hues and skin (T)")
                checkable: true; checked: s.chart === true
                onClicked: Store.cmd("chart", { on: !s.chart })
            }
            Icon {
                name: "view-dual"
                tip: Store.tr("Before / after (\\)")
                checkable: true; checked: s.compare === true
                onClicked: Store.cmd("compare", { on: !s.compare })
            }
            Icon {
                name: "dialog-warning"
                tip: Store.tr("Show clipping (C)")
                checkable: true; checked: s.overlay === true
                onClicked: Store.cmd("overlay", { on: !s.overlay })
            }
            Item { width: 6 }
            Presets {}
            // Export: the photo, or (from the arrow) a LUT of the grade.
            Row {
                spacing: 1
                Btn {
                    text: Store.tr("Export")
                    highlighted: true
                    enabled: s.hasSource === true
                    ToolTip.visible: hovered; ToolTip.text: Store.tr("Export the edited photo (Ctrl+E)"); ToolTip.delay: 600
                    onClicked: Store.cmd("exportPhoto", {})
                }
                Btn {
                    id: exportMore
                    width: 28
                    highlighted: true
                    enabled: s.hasSource === true
                    icon.source: "qrc:/icons/pan-down-symbolic.svg"; icon.color: Theme.accentInk; icon.width: 12; icon.height: 12
                    ToolTip.visible: hovered; ToolTip.text: Store.tr("Photo and LUT formats"); ToolTip.delay: 600
                    onClicked: exportMenu.popup(exportMore, 0, exportMore.height)
                }
                Menu {
                    id: exportMenu
                    Repeater {
                        model: (s.export || {}).formats || []
                        MenuItem { text: modelData.name + "…"; onTriggered: Store.cmd("exportPhoto", { format: modelData.tag }) }
                    }
                    MenuSeparator {}
                    MenuItem { text: Store.tr("33³ .cube · cameras and monitors"); onTriggered: Store.cmd("exportLut", { format: "cube", size: 33, what: "grade" }) }
                    MenuItem { text: Store.tr("65³ .cube · grading and editing"); onTriggered: Store.cmd("exportLut", { format: "cube", size: 65, what: "grade" }) }
                    MenuItem { text: Store.tr("129³ .cube · maximum precision"); onTriggered: Store.cmd("exportLut", { format: "cube", size: 129, what: "grade" }) }
                    MenuSeparator {}
                    MenuItem { text: Store.tr("Panasonic VLT · 17³ .vlt"); onTriggered: Store.cmd("exportLut", { format: "vlt", size: 17, what: "grade" }) }
                    MenuItem { text: Store.tr("HALD CLUT · 64³ .png"); onTriggered: Store.cmd("exportLut", { format: "png", size: 64, what: "grade" }) }
                    MenuSeparator {}
                    MenuItem { text: Store.tr("HALD identity · .png"); onTriggered: Store.cmd("exportLut", { format: "png", size: 64, what: "identity" }) }
                }
            }
            Icon { id: menuBtn; name: "open-menu"; tip: Store.tr("Main Menu"); onClicked: mainMenu.popup(menuBtn, 0, menuBtn.height) }
            Icon {
                name: "sidebar-show-right"
                tip: Store.tr("Show panel")
                checkable: true; checked: sidebar.visible
                onClicked: sidebar.visible = !sidebar.visible
            }
            Menu {
                id: mainMenu
                Menu {
                    title: Store.tr("Preview background")
                    Repeater {
                        model: [["default", Store.tr("Default")], ["black", Store.tr("Black")], ["white", Store.tr("White")]]
                        MenuItem {
                            text: modelData[1]
                            checkable: true
                            checked: (s.prefs || {}).background === modelData[0]
                            onTriggered: Store.cmd("background", { name: modelData[0] })
                        }
                    }
                }
                MenuSeparator {}
                MenuItem { text: Store.tr("Copy Edits"); onTriggered: Store.cmd("copyEdits") }
                MenuItem { text: Store.tr("Paste Edits"); onTriggered: Store.cmd("pasteEdits") }
                MenuItem { text: Store.tr("Paste Edits to All Photos"); onTriggered: Store.cmd("pasteEditsAll") }
                MenuItem { text: Store.tr("Reset All Edits"); onTriggered: Store.cmd("resetAll") }
                MenuSeparator {}
                MenuItem { text: Store.tr("Merge Brackets to HDR…"); onTriggered: Store.cmd("mergeHdr") }
                MenuSeparator {}
                MenuItem { text: Store.tr("Fullscreen"); onTriggered: win.toggleFull() }
                MenuItem { text: Store.tr("Preferences"); onTriggered: prefs.open() }
                MenuItem { text: Store.tr("Keyboard Shortcuts"); onTriggered: keys.open() }
                MenuItem { text: Store.tr("About RawVibes"); onTriggered: about.open() }
            }
        }
    }

    SplitView {
        Layout.fillWidth: true
        Layout.fillHeight: true
        orientation: Qt.Horizontal
        handle: Rectangle {
            implicitWidth: 5
            color: SplitHandle.pressed || SplitHandle.hovered ? Theme.line : Theme.panel
            Accessible.name: Store.tr("Resize the sidebar")
        }
        ColumnLayout {
            SplitView.fillWidth: true
            spacing: 0
            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: s.tab === "lab" ? 2 : (s.grid ? 1 : 0)
                PhotoCanvas {}
                RollGrid {}
                LabCanvas {}
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Theme.line; visible: strip.visible }
            Filmstrip {
                id: strip
                Layout.fillWidth: true
                visible: !layout.stripHidden && (s.roll || []).length > 0
            }
        }
        Rectangle {
            id: sidebar
            SplitView.preferredWidth: layout.sidebar
            SplitView.minimumWidth: 300
            SplitView.maximumWidth: 720
            onWidthChanged: if (visible && width >= 300) layout.sidebar = width
            color: Theme.panel
            Sidebar { anchors.fill: parent; pinned: layout.scopesPinned; onPinToggled: layout.scopesPinned = !layout.scopesPinned }
        }
    }

    }
    function toggleFull() { win.visibility = win.visibility === Window.FullScreen ? Window.Windowed : Window.FullScreen }

    Toasts {
        anchors { horizontalCenter: parent.horizontalCenter; bottom: parent.bottom; bottomMargin: strip.visible ? strip.height + 16 : 16 }
        z: 10
    }
    Dialogs {}
    ExportDialog {}
    PrefsDialog { id: prefs }
    ShortcutsDialog { id: keys }
    Dialog {
        id: about
        modal: true
        anchors.centerIn: Overlay.overlay
        width: 420
        background: Rectangle { color: Theme.panel; radius: 12; border.color: Theme.line }
        standardButtons: Dialog.Close
        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            Label { text: "RawVibes"; color: Theme.text; font.pixelSize: 22; font.weight: Font.Bold }
            Caption { text: Store.tr("RAW development, colour grading and 3D .cube LUT creation, GPU accelerated (Vulkan/Metal).") }
            Caption { text: "GPL-3.0-or-later · Qt 6" }
        }
    }

    // Keyboard: as the GTK interface. Single keys stay with a text field that has focus.
    component Key: Shortcut { context: Qt.ApplicationShortcut }
    Key { sequences: [StandardKey.Open]; onActivated: Store.cmd("open") }
    Key { sequence: "Ctrl+Z"; onActivated: Store.cmd("undo") }
    Key { sequences: ["Ctrl+Shift+Z", "Ctrl+Y"]; onActivated: Store.cmd("redo") }
    Key { sequence: "Ctrl+Shift+C"; onActivated: Store.cmd("copyEdits") }
    Key { sequence: "Ctrl+Shift+V"; onActivated: Store.cmd("pasteEdits") }
    Key { sequence: "F11"; onActivated: win.toggleFull() }
    Repeater {
        model: 6
        Item { Shortcut { context: Qt.ApplicationShortcut; sequence: String(index); onActivated: Store.cmd("rate", { n: index, toggle: false }) } }
    }
    Repeater {
        model: [["6", "Red"], ["7", "Yellow"], ["8", "Green"], ["9", "Blue"]]
        Item { Shortcut { context: Qt.ApplicationShortcut; sequence: modelData[0]; onActivated: Store.cmd("mark", { label: modelData[1] }) } }
    }
    Key { sequence: "Ctrl+E"; onActivated: Store.cmd("exportPhoto", {}) }
    Key { sequence: "Ctrl+Shift+E"; onActivated: Store.cmd("exportLut", { format: "cube", size: 33, what: "grade" }) }
    Key { sequence: "T"; onActivated: Store.cmd("chart", { on: !s.chart }) }
    Key { sequence: "\\"; onActivated: Store.cmd("compare", { on: !s.compare }) }
    Key { sequence: "C"; onActivated: Store.cmd("overlay", { on: !s.overlay }) }
    Key { sequence: "G"; onActivated: if ((s.roll || []).length > 1) Store.cmd("grid", { on: !s.grid }) }
    Key { sequence: "K"; onActivated: Store.cmd("skin", { on: !s.skin }) }
    Key { sequence: "Ctrl+S"; onActivated: Store.cmd("presetSave") }
    Key { sequence: "Alt+1"; onActivated: Store.cmd("tab", { tab: "photo" }) }
    Key { sequence: "Alt+2"; onActivated: Store.cmd("tab", { tab: "grade" }) }
    Key { sequence: "Alt+3"; onActivated: Store.cmd("tab", { tab: "meta" }) }
    Key { sequence: "Alt+4"; onActivated: Store.cmd("tab", { tab: "lab" }) }
    Key { sequence: "Ctrl+F"; onActivated: { Store.cmd("tab", { tab: "grade" }); Store.cmd("browse", { on: true }) } }
    Key { sequence: "F5"; onActivated: Store.cmd("reload") }
    Key { sequence: "B"; onActivated: Store.cmd("browse", { on: !s.browsing }) }
    Key { sequence: "Ctrl+,"; onActivated: prefs.open() }
    Key { sequence: "R"; onActivated: Store.cmd("cropEdit", { on: !(s.crop && s.crop.editing) }) }
    Key { sequence: "Escape"; onActivated: { Store.levelling = false; Store.cmd("escape") } }
    Key { sequence: "Return"; onActivated: Store.cmd("cropEdit", { on: false }) }
    Key { sequence: "Ctrl+0"; onActivated: Store.cmd("zoom", { cmd: "fit" }) }
    Key { sequence: "Ctrl+1"; onActivated: Store.cmd("zoom", { cmd: "actual" }) }
    Key { sequences: [StandardKey.ZoomIn, "Ctrl+="]; onActivated: Store.cmd("zoom", { cmd: "in" }) }
    Key { sequences: [StandardKey.ZoomOut]; onActivated: Store.cmd("zoom", { cmd: "out" }) }
    Key { sequence: "Ctrl+?"; onActivated: keys.open() }
}
