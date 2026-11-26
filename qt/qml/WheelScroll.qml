import QtQuick

// The mouse wheel scrolls `flick` at once, a notch at a time, as GTK does: Qt's own wheel animates
// behind it. Inside a ListView or GridView it takes the view itself; in a ScrollView, pass its contentItem.
WheelHandler {
    property Item flick: parent
    target: null
    onWheel: (e) => {
        // A touchpad gives pixels, a wheel notch 120 units: 60 px a notch. Vertical wheels scroll
        // horizontal views (the filmstrip) too.
        const px = e.pixelDelta.y !== 0 || e.pixelDelta.x !== 0
        const d = px ? (e.pixelDelta.y !== 0 ? e.pixelDelta.y : e.pixelDelta.x)
                     : (e.angleDelta.y !== 0 ? e.angleDelta.y : e.angleDelta.x) / 120 * 60
        const f = flick
        if (f.contentHeight > f.height + 1)
            f.contentY = Math.max(f.originY, Math.min(f.originY + f.contentHeight - f.height, f.contentY - d))
        else if (f.contentWidth > f.width + 1)
            f.contentX = Math.max(f.originX, Math.min(f.originX + f.contentWidth - f.width, f.contentX - d))
    }
}
