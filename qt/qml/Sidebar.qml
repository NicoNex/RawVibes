import QtQuick
import QtQuick.Controls
import QtQuick.Controls.impl
import QtQuick.Layouts
import RawVibes

// The tabs (Photo, Grade, Metadata, LUT Lab), the scopes (pinned on top, or scrolling with the tab) and the tab's controls.
ColumnLayout {
    id: side
    readonly property var s: Store.s
    property bool pinned: true
    signal pinToggled()
    spacing: 0

    TabBar {
        id: tabs
        Layout.fillWidth: true
        readonly property var keys: ["photo", "grade", "meta", "lab"]
        currentIndex: Math.max(0, keys.indexOf(s.tab))
        background: Rectangle { color: Theme.panel; Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.line } }
        Repeater {
            model: [[Store.tr("Photo"), "camera-shutter"], [Store.tr("Grade"), "rawvibes-grade"], [Store.tr("Metadata"), "tag-outline"], [Store.tr("LUT Lab"), "rawvibes-lut"]]
            TabButton {
                text: modelData[0]
                onClicked: Store.cmd("tab", { tab: tabs.keys[index] })
                icon.source: "qrc:/icons/" + modelData[1] + "-symbolic.svg"
                icon.width: 16; icon.height: 16
                icon.color: checked ? Theme.accent : Theme.dim
                display: AbstractButton.TextUnderIcon
                font.pixelSize: Theme.small
                ToolTip.visible: hovered; ToolTip.text: modelData[0] + " (Alt+" + (index + 1) + ")"; ToolTip.delay: 800
                contentItem: IconLabel {
                    icon: parent.icon; text: parent.text; display: parent.display; spacing: 2
                    color: parent.checked ? Theme.text : Theme.dim; font: parent.font
                }
                background: Rectangle {
                    color: parent.hovered ? Theme.raised : "transparent"
                    Rectangle { visible: parent.parent.checked; anchors.bottom: parent.bottom; width: parent.width; height: 2; color: Theme.accent }
                }
            }
        }
    }

    ColumnLayout {
        Layout.fillWidth: true
        Layout.margins: 12
        Layout.bottomMargin: 0
        visible: side.pinned && s.tab !== "lab" && !(s.browsing && s.tab === "grade")
        RowLayout {
            Layout.fillWidth: true
            Label { text: Store.tr("Scopes"); color: Theme.text; font.pixelSize: Theme.heading; font.weight: Font.DemiBold; Layout.fillWidth: true }
            Icon { name: "view-pin"; tip: Store.tr("Keep scopes in view while scrolling"); checkable: true; checked: side.pinned; onClicked: side.pinToggled() }
        }
        Scopes { Layout.fillWidth: true }
    }

    // The LUT browser takes the Grade tab's place while it is open.
    LutBrowser {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.margins: 12
        visible: s.browsing === true && s.tab === "grade"
    }

    ScrollView {
        id: scroll
        Layout.fillWidth: true
        Layout.fillHeight: true
        visible: !(s.browsing === true && s.tab === "grade")
        contentWidth: availableWidth
        clip: true
        ColumnLayout {
            width: scroll.availableWidth - 24
            x: 12
            spacing: 6
            Item { height: 6 }
            ColumnLayout {
                Layout.fillWidth: true
                visible: !side.pinned && s.tab !== "lab"
                RowLayout {
                    Layout.fillWidth: true
                    Label { text: Store.tr("Scopes"); color: Theme.text; font.pixelSize: Theme.heading; font.weight: Font.DemiBold; Layout.fillWidth: true }
                    Icon { name: "view-pin"; tip: Store.tr("Keep scopes in view while scrolling"); checkable: true; checked: side.pinned; onClicked: side.pinToggled() }
                }
                Scopes { Layout.fillWidth: true; visible: !side.pinned }
            }
            PhotoPage { Layout.fillWidth: true; visible: s.tab === "photo" }
            GradePage { Layout.fillWidth: true; visible: s.tab === "grade" }
            MetaPage { Layout.fillWidth: true; visible: s.tab === "meta" }
            LabPage { Layout.fillWidth: true; visible: s.tab === "lab" }
            Btn {
                Layout.alignment: Qt.AlignHCenter
                Layout.topMargin: 6
                Layout.bottomMargin: 18
                visible: s.tab === "photo" || s.tab === "grade"
                text: Store.tr("Reset all")
                flat: true
                onClicked: Store.cmd("resetAll")
            }
        }
    }
}
