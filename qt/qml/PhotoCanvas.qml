import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Shapes
import RawVibes
import RawVibes.Hdr

// The photo as graded, and everything done on it: zoom and pan, the before/after split,
// the white-balance picker, the crop frame and the level line.
Rectangle {
    id: canvas
    readonly property var s: Store.s
    readonly property int margin: 24
    readonly property real dpr: Screen.devicePixelRatio
    color: s.prefs && s.prefs.background === "black" ? "#000000"
         : s.prefs && s.prefs.background === "white" ? "#f4f4f4" : Theme.canvas
    clip: true

    // The picture area in device pixels: what the GPU renders the view to.
    function report() {
        Store.cmd("canvasSize", { w: Math.max(1, (width - 2 * margin) * dpr), h: Math.max(1, (height - 2 * margin) * dpr) })
    }
    onWidthChanged: resize.restart()
    onHeightChanged: resize.restart()
    Component.onCompleted: report()
    Timer { id: resize; interval: 60; onTriggered: canvas.report() }

    function area(x, y) { return { x: (x - margin) * dpr, y: (y - margin) * dpr } }

    Image {
        id: photo
        anchors { fill: parent; margins: canvas.margin }
        source: Store.img("photo")
        // Hidden only under an HDR window that shows HDR: while it checks its swapchain, the photo stays.
        visible: s.canvas === "image" && !(hdrView.visible && hdr.hdr)
        fillMode: Image.PreserveAspectFit
        cache: false
        smooth: true
        asynchronous: false
        // Keeps the last frame on screen while the next one is decoded: no flash between frames.
        retainWhileLoading: true
    }

    // Where the photo is drawn inside `photo`, for the overlays.
    readonly property rect shown: Qt.rect(photo.x + (photo.width - photo.paintedWidth) / 2,
                                          photo.y + (photo.height - photo.paintedHeight) / 2,
                                          photo.paintedWidth, photo.paintedHeight)

    // Before | after: the ungraded side is on the left of the line.
    Rectangle {
        visible: s.compare && s.canvas === "image"
        x: canvas.shown.x + s.split * canvas.shown.width - 1
        y: canvas.shown.y; width: 2; height: canvas.shown.height
        color: Theme.text
        opacity: 0.9
        Rectangle {
            anchors.centerIn: parent
            width: 22; height: 22; radius: 11
            color: Theme.panel; border.color: Theme.text
            Text { anchors.centerIn: parent; text: "⇔"; color: Theme.text; font.pixelSize: 12 }
        }
    }

    Column {
        anchors.centerIn: parent
        spacing: 12
        visible: s.canvas !== "image"
        BusyIndicator { anchors.horizontalCenter: parent.horizontalCenter; running: s.canvas === "loading"; visible: running }
        Label {
            anchors.horizontalCenter: parent.horizontalCenter
            visible: s.canvas === "empty"
            text: Store.tr("Open a photo")
            color: Theme.text; font.pixelSize: 20; font.weight: Font.DemiBold
        }
        Label {
            anchors.horizontalCenter: parent.horizontalCenter
            visible: s.canvas === "empty"
            text: Store.tr("Drop camera RAWs, JPEGs, TIFFs or PNGs here, or choose files. Open several to edit them together.\nEvery grade can also be exported as a .cube LUT.")
            color: Theme.dim
            horizontalAlignment: Text.AlignHCenter
        }
        Btn {
            anchors.horizontalCenter: parent.horizontalCenter
            visible: s.canvas === "empty"
            text: Store.tr("Open Photo…")
            onClicked: Store.cmd("open")
        }
    }

    CanvasPointer { anchors.fill: parent; canvas: canvas }

    // The HDR rendition on an HDR surface laid exactly over the photo; the SDR photo stays under it.
    // A native window sits above every Qt Quick item, so it steps aside while anything is drawn on the photo.
    HdrWindow {
        id: hdr
        color: "black"
        onHdrChanged: {
            probe.running = false
            Store.cmd("hdrWhite", { hdr: hdr.hdr, v: hdr.white, pq: hdr.pq })
        }
        Image {
            anchors.fill: parent
            // Only the HDR rendition: an SDR frame on an HDR10 swapchain would read as PQ.
            source: hdr.hdr ? Store.img("hdr") : ""
            fillMode: Image.Stretch
            cache: false
            smooth: true
            retainWhileLoading: true
        }
        CanvasPointer { anchors.fill: parent; canvas: canvas; ox: canvas.shown.x; oy: canvas.shown.y }
        DropArea { anchors.fill: parent; onDropped: (drop) => Store.dropPaths(drop.urls) }
    }
    WindowContainer {
        id: hdrView
        window: hdr
        x: canvas.shown.x; y: canvas.shown.y
        width: canvas.shown.width; height: canvas.shown.height
        // Shown while it checks its swapchain (one frame), then only with HDR frames to show:
        // an SDR swapchain steps aside for the SDR photo under it.
        visible: s.hdrPreview && s.canvas === "image" && (probe.running || (hdr.hdr && Store.f.hdr > 0))
                 && !(s.crop && s.crop.editing) && !s.compare && !Store.covered
    }
    // Checks the swapchain when the preview is switched on and when the window changes screen;
    // a window that never draws (no screen to show it on) never answers and counts as SDR.
    Timer { id: probe; interval: 2000 }
    function reprobe() {
        if (!s.hdrPreview || !(Store.f.photo > 0)) return
        hdr.reprobe()
        probe.restart()
    }
    readonly property bool previewOn: s.hdrPreview && Store.f.photo > 0
    onPreviewOnChanged: reprobe()
    Connections { target: canvas.Window.window; function onScreenChanged() { canvas.reprobe() } }
    // On Wayland the HDR window is a subsurface its parent commits: a new HDR frame shows once the
    // editor's window draws too.
    readonly property real hdrFrame: Store.f.hdr || 0
    onHdrFrameChanged: if (Qt.platform.os === "linux" && hdrView.visible && canvas.Window.window) canvas.Window.window.update()

    // The crop over the whole straightened frame, while editing it.
    Item {
        id: crop
        visible: s.crop.editing && s.canvas === "image"
        anchors.fill: parent
        readonly property var r: s.crop.rect
        readonly property rect box: Qt.rect(canvas.shown.x + r[0] * canvas.shown.width, canvas.shown.y + r[1] * canvas.shown.height,
                                            r[2] * canvas.shown.width, r[3] * canvas.shown.height)
        Shape {
            anchors.fill: parent
            ShapePath {
                fillColor: "#8c000000"; strokeColor: "transparent"; fillRule: ShapePath.OddEvenFill
                PathRectangle { x: canvas.shown.x; y: canvas.shown.y; width: canvas.shown.width; height: canvas.shown.height }
                PathRectangle { x: crop.box.x; y: crop.box.y; width: crop.box.width; height: crop.box.height }
            }
            ShapePath {
                strokeColor: "#73ffffff"; strokeWidth: 1; fillColor: "transparent"
                PathMove { x: crop.box.x + crop.box.width / 3; y: crop.box.y }
                PathLine { relativeX: 0; relativeY: crop.box.height }
                PathMove { x: crop.box.x + crop.box.width * 2 / 3; y: crop.box.y }
                PathLine { relativeX: 0; relativeY: crop.box.height }
                PathMove { x: crop.box.x; y: crop.box.y + crop.box.height / 3 }
                PathLine { relativeX: crop.box.width; relativeY: 0 }
                PathMove { x: crop.box.x; y: crop.box.y + crop.box.height * 2 / 3 }
                PathLine { relativeX: crop.box.width; relativeY: 0 }
            }
        }
        Rectangle {
            x: crop.box.x; y: crop.box.y; width: crop.box.width; height: crop.box.height
            color: "transparent"; border.color: "#f2ffffff"; border.width: 1.5
        }
        Repeater {
            model: [[0, 0], [0.5, 0], [1, 0], [0, 0.5], [1, 0.5], [0, 1], [0.5, 1], [1, 1]]
            Rectangle {
                x: crop.box.x + crop.box.width * modelData[0] - 4
                y: crop.box.y + crop.box.height * modelData[1] - 4
                width: 8; height: 8; color: "white"; border.color: "#80000000"
            }
        }
        // The level line, while drawn.
        Shape {
            visible: levelDrag.active
            ShapePath {
                strokeColor: Theme.accent; strokeWidth: 2
                startX: levelDrag.from.x; startY: levelDrag.from.y
                PathLine { x: levelDrag.to.x; y: levelDrag.to.y }
            }
        }
        MouseArea {
            id: levelDrag
            anchors.fill: parent
            property point from
            property point to
            property bool active: false
            cursorShape: !!(s.crop && s.crop.levelling) ? Qt.CrossCursor : Qt.ArrowCursor
            onPressed: (e) => {
                from = to = Qt.point(e.x, e.y)
                active = !!(s.crop && s.crop.levelling)
                if (active) return
                const r = canvas.shown
                Store.cmd("cropPress", { x: (e.x - r.x) / r.width, y: (e.y - r.y) / r.height, tx: 12 / r.width, ty: 12 / r.height })
            }
            onPositionChanged: (e) => {
                to = Qt.point(e.x, e.y)
                if (active) return
                Store.cmd("cropDrag", { dx: (e.x - from.x) / canvas.shown.width, dy: (e.y - from.y) / canvas.shown.height })
            }
            onReleased: {
                if (active) Store.cmd("cropLevel", { dx: to.x - from.x, dy: to.y - from.y })
                active = false
            }
        }
    }

    // What the clipping stripes mean, while they show.
    Rectangle {
        visible: s.overlay && s.canvas === "image"
        anchors { horizontalCenter: parent.horizontalCenter; top: parent.top; topMargin: 12 }
        width: legend.implicitWidth + 24; height: 32; radius: 16
        color: "#e0202020"
        Row {
            id: legend
            anchors.centerIn: parent
            spacing: 14
            Repeater {
                model: [["#ff2929", Store.tr("Clipped highlights")], ["#2e6bff", Store.tr("Crushed blacks")], ["#ffdb1a", Store.tr("Out of gamut")]]
                Row {
                    spacing: 6
                    Rectangle { width: 10; height: 10; radius: 2; color: modelData[0]; anchors.verticalCenter: parent.verticalCenter }
                    Label { text: modelData[1]; color: Theme.text; font.pixelSize: Theme.small }
                }
            }
        }
    }

    // Zoom controls, bottom right.
    Rectangle {
        visible: s.canvas === "image" && !s.chart
        anchors { right: parent.right; bottom: parent.bottom; margins: 12 }
        width: zoomRow.implicitWidth + 8; height: 34; radius: 17
        color: "#e0202020"
        Row {
            id: zoomRow
            anchors.centerIn: parent
            Icon { name: "zoom-out"; tip: Store.tr("Zoom out (Ctrl+−)"); onClicked: Store.cmd("zoom", { cmd: "out" }) }
            Label {
                width: 48; height: 30
                horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter
                text: (s.zoomPercent || 0) + "%"; color: Theme.text; font.features: { "tnum": 1 }
            }
            Icon { name: "zoom-in"; tip: Store.tr("Zoom in (Ctrl++)"); onClicked: Store.cmd("zoom", { cmd: "in" }) }
            Icon { name: "zoom-fit-best"; tip: Store.tr("Fit (Ctrl+0)"); onClicked: Store.cmd("zoom", { cmd: "fit" }) }
            Icon { name: "zoom-original"; tip: Store.tr("Actual size (Ctrl+1)"); onClicked: Store.cmd("zoom", { cmd: "actual" }) }
        }
    }

    DropArea {
        anchors.fill: parent
        onDropped: (drop) => {
            Store.dropPaths(drop.urls)
        }
    }
}
