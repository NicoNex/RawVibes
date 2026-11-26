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
    // Linux: the header bar is the title bar, as in GTK. Qt draws no decorations on a Vulkan window
    // (the HDR swapchain needs Vulkan), and GNOME none of its own.
    readonly property bool ownFrame: Qt.platform.os === "linux"
    flags: ownFrame ? Qt.Window | Qt.FramelessWindowHint : Qt.Window
    readonly property bool maximized: visibility === Window.Maximized || visibility === Window.FullScreen
    function toggleMaximized() { if (win.maximized) win.showNormal(); else win.showMaximized() }
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
    onClosing: Backend.shutdown()

    // Debugging aid, with RAWVIBES_DEBUG set: `--shot=<file.png>` saves the window once it settles, then quits;
    // `--tab=<tab>` opens a tab; `--do=<command>:<json>` runs commands; `--wait=<ms>` delays the shot.
    function arg(name) { return Store.c.debug ? (Qt.application.arguments.find(a => a.startsWith(name + "=")) || "").substring(name.length + 1) : "" }
    readonly property string shot: arg("--shot")
    Component.onCompleted: {
        if (!Store.c.debug) return
        const tab = arg("--tab")
        if (tab) Store.cmd("tab", { tab: tab })
        // `--do=<command>:<json>`: commands run once the photo has loaded (screenshots of a state).
        acts.list = Qt.application.arguments.filter(a => a.startsWith("--do=")).map(a => a.substring(5))
    }
    Timer {
        id: acts
        property var list: []
        running: list.length > 0
        interval: 3500
        onTriggered: {
            for (const a of list) {
                const i = a.indexOf(":")
                Store.cmd(i < 0 ? a : a.substring(0, i), i < 0 ? {} : JSON.parse(a.substring(i + 1)))
            }
        }
    }
    Timer {
        running: win.shot !== ""
        interval: Number(arg("--wait") || 6000)
        onTriggered: root.grabToImage(r => { r.saveToFile(win.shot); Backend.shutdown(); Qt.quit() })
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
        // Under the buttons: dragging the bar moves the window, a double-click maximizes it.
        Item {
            anchors.fill: parent
            enabled: win.ownFrame
            DragHandler { target: null; onActiveChanged: if (active) win.startSystemMove() }
            TapHandler { onDoubleTapped: win.toggleMaximized() }
        }
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
                        model: Store.c.formats
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
            Row {
                visible: win.ownFrame
                leftPadding: 8
                spacing: 4
                Icon { name: "window-minimize"; tip: Store.tr("Minimize"); onClicked: win.showMinimized() }
                Icon {
                    name: win.maximized ? "window-restore" : "window-maximize"
                    tip: win.maximized ? Store.tr("Restore") : Store.tr("Maximize")
                    onClicked: win.toggleMaximized()
                }
                Icon { name: "window-close"; tip: Store.tr("Close"); onClicked: win.close() }
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
            // Under the photo, as in GTK: the filmstrip's collapse toggle.
            Rectangle {
                Layout.fillWidth: true
                height: 26
                visible: (s.roll || []).length > 0
                color: Theme.panel
                Rectangle { width: parent.width; height: 1; color: Theme.line }
                Icon {
                    anchors { right: parent.right; rightMargin: 8; verticalCenter: parent.verticalCenter }
                    implicitWidth: 24; implicitHeight: 24; size: 14
                    name: layout.stripHidden ? "go-up" : "go-down"
                    tip: layout.stripHidden ? Store.tr("Show the filmstrip") : Store.tr("Hide the filmstrip")
                    onClicked: layout.stripHidden = !layout.stripHidden
                }
            }
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
    // Without the system's frame: its edges and corners resize the window.
    component Grip: Item {
        property int edges
        z: 100
        visible: win.ownFrame && !win.maximized
        HoverHandler {
            cursorShape: edges === (Qt.LeftEdge | Qt.TopEdge) || edges === (Qt.RightEdge | Qt.BottomEdge) ? Qt.SizeFDiagCursor
                       : edges === (Qt.RightEdge | Qt.TopEdge) || edges === (Qt.LeftEdge | Qt.BottomEdge) ? Qt.SizeBDiagCursor
                       : edges & (Qt.LeftEdge | Qt.RightEdge) ? Qt.SizeHorCursor : Qt.SizeVerCursor
        }
        DragHandler { target: null; onActiveChanged: if (active) win.startSystemResize(parent.edges) }
    }
    Grip { edges: Qt.LeftEdge; width: 5; anchors { left: parent.left; top: parent.top; bottom: parent.bottom } }
    Grip { edges: Qt.RightEdge; width: 5; anchors { right: parent.right; top: parent.top; bottom: parent.bottom } }
    Grip { edges: Qt.TopEdge; height: 5; anchors { left: parent.left; right: parent.right; top: parent.top } }
    Grip { edges: Qt.BottomEdge; height: 5; anchors { left: parent.left; right: parent.right; bottom: parent.bottom } }
    Grip { edges: Qt.LeftEdge | Qt.TopEdge; width: 10; height: 10; anchors { left: parent.left; top: parent.top } }
    Grip { edges: Qt.RightEdge | Qt.TopEdge; width: 10; height: 10; anchors { right: parent.right; top: parent.top } }
    Grip { edges: Qt.LeftEdge | Qt.BottomEdge; width: 10; height: 10; anchors { left: parent.left; bottom: parent.bottom } }
    Grip { edges: Qt.RightEdge | Qt.BottomEdge; width: 10; height: 10; anchors { right: parent.right; bottom: parent.bottom } }

    function toggleFull() { win.visibility = win.visibility === Window.FullScreen ? Window.Windowed : Window.FullScreen }

    Binding {
        target: Store; property: "covered"
        value: win.overlayOpen || toasts.count > 0
    }
    Toasts {
        id: toasts
        anchors { horizontalCenter: parent.horizontalCenter; bottom: parent.bottom; bottomMargin: strip.visible ? strip.height + 16 : 16 }
        z: 10
    }
    Dialogs {}
    ExportDialog {}
    PrefsDialog { id: prefs }
    ShortcutsDialog { id: keys; table: win.shortcuts }
    DialogShell {
        id: about
        width: 420
        standardButtons: Dialog.Close
        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            Label { text: "RawVibes"; color: Theme.text; font.pixelSize: 22; font.weight: Font.Bold }
            Caption { text: Store.tr("RAW development, colour grading and 3D .cube LUT creation, GPU accelerated (Vulkan/Metal).") }
            Caption { text: "GPL-3.0-or-later · Qt 6" }
        }
    }

    // Keyboard, as the GTK interface: one table for the shortcuts and the help that lists them.
    // `shown` is how the help writes the keys (empty: not listed); `when` gates a shortcut.
    readonly property var shortcuts: [
        { keys: [StandardKey.Open], shown: "Ctrl+O", label: Store.tr("Open Photo"), act: () => Store.cmd("open") },
        { keys: ["Ctrl+Z"], shown: "Ctrl+Z", label: Store.tr("Undo"), act: () => Store.cmd("undo") },
        { keys: ["Ctrl+Shift+Z", "Ctrl+Y"], shown: "Ctrl+Shift+Z", label: Store.tr("Redo"), act: () => Store.cmd("redo") },
        { keys: ["Ctrl+Shift+C"], shown: "Ctrl+Shift+C", label: Store.tr("Copy edits"), act: () => Store.cmd("copyEdits") },
        { keys: ["Ctrl+Shift+V"], shown: "Ctrl+Shift+V", label: Store.tr("Paste edits"), act: () => Store.cmd("pasteEdits") },
        { keys: ["1", "2", "3", "4", "5"], shown: "1–5", label: Store.tr("Stars"), act: (k) => Store.cmd("rate", { n: Number(k), toggle: false }) },
        { keys: ["0"], shown: "0", label: Store.tr("No stars"), act: () => Store.cmd("rate", { n: 0, toggle: false }) },
        { keys: ["6", "7", "8", "9"], shown: "6–9", label: Store.tr("Colour label: red, yellow, green, blue"),
          act: (k) => Store.cmd("mark", { label: ["Red", "Yellow", "Green", "Blue"][Number(k) - 6] }) },
        { keys: ["F11"], shown: "F11", label: Store.tr("Fullscreen"), act: () => win.toggleFull() },
        { keys: ["Ctrl+E"], shown: "Ctrl+E", label: Store.tr("Export photo"), act: () => Store.cmd("exportPhoto", {}) },
        { keys: ["Ctrl+Shift+E"], shown: "Ctrl+Shift+E", label: Store.tr("Export LUT · 33³ .cube"), act: () => Store.cmd("exportLut", { format: "cube", size: 33, what: "grade" }) },
        { keys: ["\\"], shown: "\\", label: Store.tr("Before / after"), act: () => Store.cmd("compare", { on: !s.compare }) },
        { keys: ["T"], shown: "T", label: Store.tr("Test chart"), act: () => Store.cmd("chart", { on: !s.chart }) },
        { keys: ["C"], shown: "C", label: Store.tr("Show clipping"), act: () => Store.cmd("overlay", { on: !s.overlay }) },
        { keys: ["K"], shown: "K", label: Store.tr("Skin tone line"), act: () => Store.cmd("skin", { on: !s.skin }) },
        { keys: ["G"], shown: "G", label: Store.tr("Grid of all photos"), act: () => { if ((s.roll || []).length > 1) Store.cmd("grid", { on: !s.grid }) } },
        { keys: ["R"], shown: "R", label: Store.tr("Edit crop"), act: () => Store.cmd("cropEdit", { on: !(s.crop && s.crop.editing) }) },
        // Escape and Enter belong to a focused field, list or open dialog first (Qt gives shortcuts these keys before them).
        { keys: ["Escape"], shown: "Esc", label: Store.tr("Cancel crop"), when: () => win.keysFree, act: () => Store.cmd("escape") },
        { keys: ["Return", "Enter"], shown: "Enter", label: Store.tr("Apply crop"), when: () => win.keysFree && !!(s.crop && s.crop.editing), act: () => Store.cmd("cropEdit", { on: false }) },
        { keys: ["Ctrl+0"], shown: "Ctrl+0", label: Store.tr("Zoom to fit"), act: () => Store.cmd("zoom", { cmd: "fit" }) },
        { keys: ["Ctrl+1"], shown: "Ctrl+1", label: Store.tr("Actual size"), act: () => Store.cmd("zoom", { cmd: "actual" }) },
        { keys: [StandardKey.ZoomIn, "Ctrl+="], shown: "Ctrl++", label: Store.tr("Zoom in"), act: () => Store.cmd("zoom", { cmd: "in" }) },
        { keys: [StandardKey.ZoomOut], shown: "Ctrl+−", label: Store.tr("Zoom out"), act: () => Store.cmd("zoom", { cmd: "out" }) },
        { keys: ["Ctrl+S"], shown: "Ctrl+S", label: Store.tr("Save preset"), act: () => Store.cmd("presetSave") },
        { keys: ["Alt+1"], shown: "Alt+1", label: Store.tr("Photo view"), act: () => Store.cmd("tab", { tab: "photo" }) },
        { keys: ["Alt+2"], shown: "Alt+2", label: Store.tr("Grade view"), act: () => Store.cmd("tab", { tab: "grade" }) },
        { keys: ["Alt+3"], shown: "Alt+3", label: Store.tr("Metadata view"), act: () => Store.cmd("tab", { tab: "meta" }) },
        { keys: ["Alt+4"], shown: "Alt+4", label: Store.tr("LUT Lab view"), act: () => Store.cmd("tab", { tab: "lab" }) },
        { keys: ["B"], shown: "B", label: Store.tr("Browse LUTs"), act: () => Store.cmd("browse", { on: !s.browsing }) },
        { keys: ["Ctrl+F"], shown: "Ctrl+F", label: Store.tr("Search LUTs"), act: () => { Store.cmd("tab", { tab: "grade" }); Store.cmd("browse", { on: true }) } },
        { keys: ["F5"], shown: "F5", label: Store.tr("Reload LUTs"), act: () => Store.cmd("reload") },
        { keys: ["Ctrl+,"], shown: "Ctrl+,", label: Store.tr("Preferences"), act: () => prefs.open() },
        { keys: ["Ctrl+?"], shown: "Ctrl+?", label: Store.tr("Keyboard Shortcuts"), act: () => keys.open() },
    ]
    // A menu, popup or dialog is open.
    readonly property bool overlayOpen: !!Overlay.overlay && Overlay.overlay.children.some(c => c.visible)
    readonly property bool keysFree: !(activeFocusItem && ("cursorPosition" in activeFocusItem || activeFocusItem instanceof GridView))
                                     && !win.overlayOpen
    Repeater {
        model: win.shortcuts.length
        Item {
            required property int index
            readonly property var entry: win.shortcuts[index]
            // One Shortcut per key, so an action can tell which (stars, labels).
            Repeater {
                model: entry.keys.length
                Item {
                    required property int index
                    Shortcut {
                        context: Qt.ApplicationShortcut
                        sequences: [entry.keys[index]]
                        enabled: !entry.when || entry.when()
                        onActivated: entry.act(String(entry.keys[index]))
                    }
                }
            }
        }
    }
}
