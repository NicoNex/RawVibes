import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// The Metadata tab: what the camera recorded (read only) and the photo's own title, keywords, rights and location.
ColumnLayout {
    id: mp
    readonly property var s: Store.s
    readonly property var m: s.meta || ({})
    readonly property var cam: s.camera || ["—", "—", "—", "—", "—"]
    enabled: s.hasPhoto === true
    spacing: 10

    component Field: ColumnLayout {
        property string key
        property string title
        property alias placeholderText: f.placeholderText
        Layout.fillWidth: true
        spacing: 2
        Label { text: parent.title; color: Theme.dim; font.pixelSize: Theme.small }
        TextField {
            id: f
            Layout.fillWidth: true
            text: mp.m[parent.key] || ""
            Accessible.name: parent.title
            // Each keystroke, not on focus out: a click on the filmstrip or closing the window keeps it.
            onTextEdited: Store.cmd("meta", { field: parent.key, text: text })
        }
    }
    component Fact: ColumnLayout {
        property string title
        property string value
        Layout.fillWidth: true
        spacing: 1
        Label { text: parent.title; color: Theme.dim; font.pixelSize: Theme.small }
        Label { text: parent.value; color: Theme.text; font.pixelSize: Theme.body; wrapMode: Text.Wrap; Layout.fillWidth: true }
    }
    component Group: Heading { Layout.topMargin: 6 }

    // The shot card.
    Rectangle {
        Layout.fillWidth: true
        implicitHeight: card.implicitHeight + 24
        radius: 10
        gradient: Gradient {
            orientation: Gradient.Vertical
            GradientStop { position: 0; color: Qt.rgba(0.88, 0.63, 0.25, 0.16) }
            GradientStop { position: 1; color: Theme.raised }
        }
        GridLayout {
            id: card
            anchors { fill: parent; margins: 12 }
            columns: 2
            columnSpacing: 12; rowSpacing: 8
            Fact { Layout.columnSpan: 2; title: Store.tr("Shot"); value: mp.cam[0] }
            Fact { title: Store.tr("Exposure"); value: mp.cam[1] }
            Fact { title: Store.tr("Focal length"); value: mp.cam[2] }
            Fact { title: Store.tr("Taken"); value: mp.cam[3] }
            Fact { title: Store.tr("GPS location"); value: mp.cam[4] }
        }
    }

    Group { text: Store.tr("Description") }
    Field { key: "title"; title: Store.tr("Title") }
    Field { key: "caption"; title: Store.tr("Caption") }

    Group { text: Store.tr("Keywords") }
    Flow {
        Layout.fillWidth: true
        spacing: 6
        Repeater {
            model: (mp.m.keywords || []).length
            Btn {
                readonly property string word: mp.m.keywords[index]
                text: word + "  ✕"
                flat: true
                ToolTip.visible: hovered; ToolTip.text: Store.tr("Remove “{}”").replace("{}", word); ToolTip.delay: 600
                onClicked: Store.cmd("meta", { field: "keywords", text: mp.m.keywords.filter(w => w !== word).join(", ") })
                background: Rectangle { radius: height / 2; color: parent.hovered ? Theme.line : Theme.raised }
            }
        }
    }
    TextField {
        Layout.fillWidth: true
        placeholderText: Store.tr("Add keywords…")
        onAccepted: {
            Store.cmd("meta", { field: "keywords", text: (mp.m.keywords || []).concat([text]).join(", ") })
            text = ""
        }
    }

    Group { text: Store.tr("Rights") }
    Field {
        key: "creator"
        title: m.creatorDefault ? Store.tr("{} (empty: {})").replace("{}", Store.tr("Creator")).replace("{}", m.creatorDefault) : Store.tr("Creator")
    }
    Field {
        key: "copyright"
        title: m.copyrightDefault ? Store.tr("{} (empty: {})").replace("{}", Store.tr("Copyright")).replace("{}", m.copyrightDefault) : Store.tr("Copyright")
    }

    Group { text: Store.tr("Location") }
    Field { key: "city"; title: Store.tr("City") }
    Field { key: "country"; title: Store.tr("Country") }

    Label {
        Layout.fillWidth: true
        Layout.topMargin: 6
        textFormat: Text.StyledText
        wrapMode: Text.Wrap
        color: Theme.dim; font.pixelSize: Theme.small + 1
        linkColor: Theme.accent
        text: s.policyNote
        onLinkActivated: Store.prefsRequested()
    }
}
