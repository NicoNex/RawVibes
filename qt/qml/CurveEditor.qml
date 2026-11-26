import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Curves of the grade's channels `first`..`last`: click to add a point, drag it, double- or right-click to remove it.
// Endpoints only move up and down; on the periodic hue curves both ends are the same red and move together.
ColumnLayout {
    id: ed
    property int first: 0
    property int last: 3
    property var names: []
    property string hint
    property int active: first
    readonly property var s: Store.s
    readonly property int pad: 8
    readonly property int strip: 10
    readonly property bool hue: active >= 4
    readonly property var rgb: ["#ebebeb", "#f0544f", "#5cd16b", "#5c8ffa"]
    spacing: 6

    function neutral(ch) { return (Store.c.curveNeutral || [])[ch] || [[0, 0], [1, 1]] }
    function periodic(ch) { return (Store.c.curvePeriodic || [])[ch] === true }
    function points(ch) { return (s.curves || [])[ch] || neutral(ch) }
    function colour(ch) { return ch < 4 ? rgb[ch] : rgb[0] }

    RowLayout {
        Layout.fillWidth: true
        Toggles {
            Layout.fillWidth: true
            model: ed.names
            current: ed.active - ed.first
            onPicked: (i) => { ed.active = ed.first + i; plot.requestPaint() }
        }
        Btn {
            text: Store.tr("Reset curve")
            flat: true
            onClicked: Store.cmd("curve", { ch: ed.active, points: ed.neutral(ed.active) })
        }
    }

    Canvas {
        id: plot
        Layout.fillWidth: true
        Layout.preferredHeight: 240
        Accessible.name: Store.tr("Curve editor")
        readonly property real iw: width - 2 * ed.pad
        readonly property real ih: height - 2 * ed.pad
        function sx(x) { return ed.pad + x * iw }
        function sy(y) { return ed.pad + (1 - y) * ih }
        // Repainted when the curves change, not on every state the editor publishes.
        readonly property string drawn: JSON.stringify(ed.s.curves)
        onDrawnChanged: requestPaint()
        onPaint: {
            const ctx = getContext("2d")
            ctx.reset()
            ctx.fillStyle = Theme.canvas
            ctx.fillRect(0, 0, width, height)
            if (ed.hue) {
                const strip = Store.c.hueStrip || []
                for (let i = 0; i < 72; i++) {
                    ctx.fillStyle = ed.active === 6 ? Qt.rgba((i + 0.5) / 72, (i + 0.5) / 72, (i + 0.5) / 72, 1) : strip[i]
                    ctx.fillRect(sx(i / 72), ed.pad + ih - ed.strip, iw / 72 + 0.5, ed.strip)
                }
            }
            ctx.strokeStyle = Qt.rgba(1, 1, 1, 0.08); ctx.lineWidth = 1
            ctx.beginPath()
            for (let k = 1; k < 4; k++) {
                ctx.moveTo(sx(k / 4), ed.pad); ctx.lineTo(sx(k / 4), ed.pad + ih)
                ctx.moveTo(ed.pad, sy(k / 4)); ctx.lineTo(ed.pad + iw, sy(k / 4))
            }
            const n = ed.neutral(ed.active)
            ctx.moveTo(sx(0), sy(n[0][1])); ctx.lineTo(sx(1), sy(n[1][1]))
            ctx.stroke()
            // Inactive channels faintly behind, the active one on top.
            const order = []
            for (let ch = ed.first; ch <= ed.last; ch++) if (ch !== ed.active) order.push(ch)
            order.push(ed.active)
            for (const ch of order) {
                const on = ch === ed.active
                if (!on && (ch >= 4 || JSON.stringify(ed.points(ch)) === JSON.stringify(ed.neutral(ch)))) continue
                const smp = (s.samples || [])[ch] || []
                ctx.strokeStyle = ed.colour(ch); ctx.globalAlpha = on ? 1 : 0.35; ctx.lineWidth = on ? 2 : 1.25
                ctx.beginPath()
                for (let i = 0; i < smp.length; i++) {
                    const x = sx(i / (smp.length - 1)), y = sy(smp[i])
                    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y)
                }
                ctx.stroke()
                ctx.globalAlpha = 1
                if (on) for (const p of ed.points(ch)) {
                    ctx.beginPath(); ctx.arc(sx(p[0]), sy(p[1]), 5, 0, 2 * Math.PI)
                    ctx.fillStyle = ed.colour(ch); ctx.fill()
                    ctx.strokeStyle = Qt.rgba(0, 0, 0, 0.6); ctx.lineWidth = 1.5; ctx.stroke()
                }
            }
        }

        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.LeftButton | Qt.RightButton
            // Inside the sidebar's Flickable: without this, a vertical drag hands the mouse to the scroll.
            preventStealing: true
            property int drag: -1
            property var pts: []
            property point start
            property point origin
            function hit(x, y, from, to) {
                for (let i = from; i < to; i++)
                    if (Math.hypot(plot.sx(pts[i][0]) - x, plot.sy(pts[i][1]) - y) < 12) return i
                return -1
            }
            function send() { Store.cmd("curve", { ch: ed.active, points: pts }) }
            onPressed: (e) => {
                pts = ed.points(ed.active).map(p => [p[0], p[1]])
                drag = -1
                if (e.button === Qt.RightButton) {
                    const i = hit(e.x, e.y, 1, pts.length - 1)
                    if (i >= 0) { pts.splice(i, 1); send() }
                    return
                }
                let i = hit(e.x, e.y, 0, pts.length)
                if (i < 0) {
                    const cx = Math.max(0, Math.min(1, (e.x - ed.pad) / plot.iw))
                    const cy = Math.max(0, Math.min(1, 1 - (e.y - ed.pad) / plot.ih))
                    i = Math.max(1, pts.findIndex(p => p[0] > cx))
                    if (i < 1) i = pts.length - 1
                    // A minimum gap, so neighbours never collapse onto one x.
                    if (cx - pts[i - 1][0] < 0.01 || pts[i][0] - cx < 0.01) return
                    pts.splice(i, 0, [cx, cy])
                    send()
                }
                drag = i
                start = Qt.point(e.x, e.y)
                origin = Qt.point(pts[i][0], pts[i][1])
            }
            onPositionChanged: (e) => {
                if (drag < 0) return
                const last = pts.length - 1, i = drag
                let x = pts[i][0]
                if (i !== 0 && i !== last) {
                    const lo = pts[i - 1][0] + 0.01, hi = pts[i + 1][0] - 0.01
                    x = lo < hi ? Math.max(lo, Math.min(hi, origin.x + (e.x - start.x) / plot.iw)) : (lo + hi) / 2
                }
                const y = Math.max(0, Math.min(1, origin.y - (e.y - start.y) / plot.ih))
                pts[i] = [x, y]
                if (ed.periodic(ed.active) && (i === 0 || i === last)) { pts[0][1] = y; pts[last][1] = y }
                send()
            }
            onReleased: drag = -1
            onDoubleClicked: (e) => {
                pts = ed.points(ed.active).map(p => [p[0], p[1]])
                const i = hit(e.x, e.y, 1, pts.length - 1)
                if (i >= 0) { pts.splice(i, 1); send() }
            }
        }
    }
    Caption { text: ed.hint; visible: ed.hint !== "" }
}
