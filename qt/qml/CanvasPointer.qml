import QtQuick
import RawVibes

// The canvas's pointer: zoom on the wheel, pan by dragging, double-click for 100%, click to pick a grey,
// drag the before/after split. In `canvas` coordinates, shifted by (ox, oy) when laid over part of it
// (the HDR surface, a native window that takes the pointer itself).
MouseArea {
    id: pt
    required property var canvas
    property real ox: 0
    property real oy: 0
    readonly property var s: Store.s
    enabled: !(s.crop && s.crop.editing)
    acceptedButtons: Qt.LeftButton | Qt.MiddleButton
    hoverEnabled: true
    cursorShape: s.picker ? Qt.CrossCursor : (s.zoomed ? (pressed ? Qt.ClosedHandCursor : Qt.OpenHandCursor) : Qt.ArrowCursor)
    property point last
    property bool splitting: false
    // A press that became a pan or a split drag is not a click (no grey picked where it ends).
    property real travelled: 0
    function at(e) { return canvas.area(e.x + ox, e.y + oy) }
    function setSplit(x) {
        Store.cmd("split", { x: Math.max(0, Math.min(1, (x + ox - canvas.shown.x) / canvas.shown.width)) })
    }
    onPressed: (e) => {
        last = Qt.point(e.x, e.y)
        travelled = 0
        splitting = s.compare && e.button === Qt.LeftButton
        if (splitting) setSplit(e.x)
    }
    onPositionChanged: (e) => {
        if (!pressed) return
        travelled += Math.abs(e.x - last.x) + Math.abs(e.y - last.y)
        if (splitting) { last = Qt.point(e.x, e.y); return setSplit(e.x) }
        const d = { dx: (e.x - last.x) * canvas.dpr, dy: (e.y - last.y) * canvas.dpr }
        last = Qt.point(e.x, e.y)
        Store.cmd("zoom", { cmd: "pan", dx: d.dx, dy: d.dy, button: e.buttons & Qt.MiddleButton ? 2 : 1 })
    }
    onClicked: (e) => {
        if (travelled > 4) return
        const p = at(e)
        Store.cmd("canvasClick", { x: p.x, y: p.y })
    }
    onDoubleClicked: (e) => { const p = at(e); Store.cmd("zoom", { cmd: "toggle", x: p.x, y: p.y }) }
    onWheel: (e) => {
        const p = at(e)
        Store.cmd("zoom", { cmd: "scroll", dy: -e.angleDelta.y / 120, x: p.x, y: p.y })
    }
}
