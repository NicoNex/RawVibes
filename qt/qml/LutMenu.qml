import QtQuick
import QtQuick.Controls
import RawVibes

// The LUT formats a grade (or the LUT Lab) exports to.
Menu {
    property string what: "grade"
    MenuItem { text: Store.tr("33³ .cube · cameras and monitors"); onTriggered: Store.cmd("exportLut", { format: "cube", size: 33, what: what }) }
    MenuItem { text: Store.tr("65³ .cube · grading and editing"); onTriggered: Store.cmd("exportLut", { format: "cube", size: 65, what: what }) }
    MenuItem { text: Store.tr("129³ .cube · maximum precision"); onTriggered: Store.cmd("exportLut", { format: "cube", size: 129, what: what }) }
    MenuSeparator {}
    MenuItem { text: Store.tr("Panasonic VLT · 17³ .vlt"); onTriggered: Store.cmd("exportLut", { format: "vlt", size: 17, what: what }) }
    MenuItem { text: Store.tr("HALD CLUT · 64³ .png"); onTriggered: Store.cmd("exportLut", { format: "png", size: 64, what: what }) }
}
