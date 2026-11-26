import QtQuick
import QtQuick.Controls
import RawVibes

// Hue/chroma disc: (x, y) in the unit disc, y up; the hue at atan2(y, x) is the shader's `wheel()`.
// Drag the point towards a colour; double-click puts it back in the middle.
Item {
    id: w
    property string name
    property real px: 0
    property real py: 0
    signal moved(real x, real y)
    implicitWidth: 150; implicitHeight: 150
    Accessible.name: name
    readonly property real r: Math.min(width, height) / 2 - 4

    Canvas {
        id: disc
        anchors.fill: parent
        renderStrategy: Canvas.Cooperative
        onPaint: {
            const ctx = getContext("2d")
            const cx = width / 2, cy = height / 2, r = w.r
            ctx.reset()
            const n = 120
            for (let i = 0; i < n; i++) {
                const a0 = i / n * 2 * Math.PI, a1 = (i + 1) / n * 2 * Math.PI + 0.01
                ctx.fillStyle = Qt.hsva(i / n, 0.75, 0.85, 1)
                ctx.beginPath()
                ctx.moveTo(cx, cy)
                // Screen y points down: negative angles keep hue counter-clockwise.
                ctx.arc(cx, cy, r, -a0, -a1, true)
                ctx.closePath()
                ctx.fill()
            }
            const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, r)
            g.addColorStop(0, Qt.rgba(0.5, 0.5, 0.5, 1))
            g.addColorStop(1, Qt.rgba(0.5, 0.5, 0.5, 0))
            ctx.fillStyle = g
            ctx.beginPath(); ctx.arc(cx, cy, r, 0, 2 * Math.PI); ctx.fill()
            ctx.strokeStyle = Qt.rgba(0, 0, 0, 0.35); ctx.lineWidth = 1
            ctx.beginPath(); ctx.arc(cx, cy, r, 0, 2 * Math.PI); ctx.stroke()
        }
        onWidthChanged: requestPaint()
        onHeightChanged: requestPaint()
    }
    Rectangle {
        width: 12; height: 12; radius: 6
        x: w.width / 2 + w.px * w.r - 6
        y: w.height / 2 - w.py * w.r - 6
        color: "white"; border.color: "#b3000000"; border.width: 1.5
    }
    function place(x, y) {
        let ux = (x - width / 2) / r, uy = (height / 2 - y) / r
        const len = Math.hypot(ux, uy)
        if (len > 1) { ux /= len; uy /= len }
        w.moved(ux, uy)
    }
    MouseArea {
        anchors.fill: parent
        property bool resetting: false
        onPressed: (e) => { if (!resetting) w.place(e.x, e.y) }
        onPositionChanged: (e) => { if (pressed && !resetting) w.place(e.x, e.y) }
        onReleased: resetting = false
        onDoubleClicked: { resetting = true; w.moved(0, 0) }
    }
}
