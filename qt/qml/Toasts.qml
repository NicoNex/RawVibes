import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Messages at the bottom of the window, newest last; one may carry a button (Undo, Use LUT…).
ColumnLayout {
    id: toasts
    spacing: 8
    ListModel { id: queue }
    function show(text, action, id) {
        queue.append({ text: text, action: action, cmdId: id, at: Date.now() })
        while (queue.count > 3) queue.remove(0)
    }
    Connections {
        target: Backend
        function onToast(text, action, id) { toasts.show(text, action, id) }
    }
    Repeater {
        model: queue
        Rectangle {
            id: t
            required property int index
            required property string text
            required property string action
            required property int cmdId
            Layout.alignment: Qt.AlignHCenter
            implicitWidth: Math.min(640, row.implicitWidth + 32)
            implicitHeight: row.implicitHeight + 20
            radius: 10
            color: "#f0303030"
            border.color: Theme.line
            opacity: 0
            Component.onCompleted: opacity = 1
            Behavior on opacity { NumberAnimation { duration: 150 } }
            Timer { running: true; interval: t.action !== "" ? 6000 : 4000; onTriggered: queue.remove(t.index) }
            RowLayout {
                id: row
                anchors.centerIn: parent
                width: Math.min(implicitWidth, 608)
                spacing: 12
                Label { text: t.text; color: Theme.text; wrapMode: Text.Wrap; Layout.maximumWidth: 480 }
                Btn {
                    visible: t.action !== ""
                    text: t.action
                    flat: true
                    onClicked: { Store.cmd("toastAction", { id: t.cmdId }); queue.remove(t.index) }
                    contentItem: Label { text: parent.text; color: Theme.accent; font.weight: Font.DemiBold }
                }
                Icon { name: "window-close"; size: 12; width: 22; height: 22; tip: Store.tr("Close"); onClicked: queue.remove(t.index) }
            }
        }
    }
}
