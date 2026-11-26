import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import RawVibes

// Preferences: the LUT library, how RAWs preview, and what exported photos carry.
DialogShell {
    id: dlg
    readonly property var s: Store.s
    readonly property var pr: s.prefs || ({})
    title: Store.tr("Preferences")
    width: Math.min(560, parent ? parent.width - 40 : 560)
    height: Math.min(680, parent ? parent.height - 40 : 680)
    padding: 0
    header: ColumnLayout {
        spacing: 0
        Label { text: dlg.title; color: Theme.text; font.pixelSize: 17; font.weight: Font.DemiBold; padding: 20; bottomPadding: 8 }
        Toggles {
            Layout.fillWidth: true; Layout.leftMargin: 20; Layout.rightMargin: 20
            model: [Store.tr("General"), Store.tr("Metadata")]
            current: pages.currentIndex
            onPicked: (i) => pages.currentIndex = i
        }
    }
    component Group: Heading { Layout.topMargin: 10 }
    component Entry: ColumnLayout {
        property string title
        property string key
        property string value
        Layout.fillWidth: true
        spacing: 2
        Label { text: parent.title; color: Theme.dim; font.pixelSize: Theme.small }
        TextField {
            Layout.fillWidth: true
            text: parent.value
            onEditingFinished: Store.cmd("authorDefault", { key: parent.key, text: text })
        }
    }

    ScrollView {
        anchors.fill: parent
        anchors.margins: 20
        contentWidth: availableWidth
        StackLayout {
            id: pages
            width: parent.width
            ColumnLayout {
                spacing: 10
                Group { text: Store.tr("LUT library") }
                Caption { text: Store.tr("LUT files in this folder and its subfolders appear in the LUT browser.") }
                RowLayout {
                    Layout.fillWidth: true
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0
                        Label { text: Store.tr("LUT folder"); color: Theme.text }
                        Label { text: s.lutFolder || Store.tr("No folder"); color: Theme.dim; font.pixelSize: Theme.small; elide: Text.ElideMiddle; Layout.fillWidth: true }
                    }
                    Icon { name: "folder-open"; tip: Store.tr("Choose folder"); onClicked: Store.cmd("chooseFolder") }
                }
                Group { text: Store.tr("RAW development") }
                Label { text: Store.tr("Preview while editing"); color: Theme.text }
                Choice {
                    Layout.fillWidth: true
                    model: Store.c.qualities
                    currentIndex: s.quality || 0
                    onActivated: (i) => Store.cmd("quality", { i: i })
                }
                Caption { text: Store.tr("Faster previews are softer while you drag. Automatic measures this computer's graphics card and keeps dragging smooth.") }
                Switch2 {
                    title: Store.tr("Sharpen the preview when you stop")
                    subtitle: Store.tr("Renders the full-quality frame after each change. Turn off on slower computers to keep editing responsive.")
                    checked: s.refine === true
                    onToggled: (on) => Store.cmd("refine", { on: on })
                }
                Switch2 {
                    title: Store.tr("Open RAWs as in camera")
                    subtitle: Store.tr("A RAW you have not edited yet starts from the camera's white balance and exposure.")
                    checked: s.asInCameraOnOpen === true
                    onToggled: (on) => Store.cmd("asInCameraOnOpen", { on: on })
                }
            }
            ColumnLayout {
                spacing: 10
                Group { text: Store.tr("Export") }
                Caption { text: Store.tr("Exported photos are named after the original plus this suffix (_edit when empty).") }
                Label { text: Store.tr("Metadata in exported photos"); color: Theme.text }
                Choice {
                    Layout.fillWidth: true
                    model: Store.c.policyNames
                    currentIndex: pr.policy || 0
                    onActivated: (i) => Store.cmd("metaPolicy", { tag: Store.c.policies[i].tag })
                }
                Caption { text: ((Store.c.policies || [])[pr.policy || 0] || {}).hint || "" }
                Label { text: Store.tr("File name suffix"); color: Theme.dim; font.pixelSize: Theme.small }
                TextField {
                    Layout.fillWidth: true
                    text: pr.suffix || ""
                    onTextEdited: Store.cmd("exportSuffix", { text: text })
                }
                Group { text: Store.tr("Author") }
                Caption { text: Store.tr("Written into every export whose photo leaves the field empty. In the copyright, {year} becomes the year the photo was taken and {creator} the creator.") }
                Entry { title: Store.tr("Creator"); key: "author"; value: pr.author || "" }
                Entry { title: Store.tr("Copyright"); key: "copyright"; value: pr.copyright || "" }
                Entry { title: Store.tr("Email"); key: "email"; value: pr.email || "" }
                Entry { title: Store.tr("Website"); key: "website"; value: pr.website || "" }
            }
        }
    }
}
