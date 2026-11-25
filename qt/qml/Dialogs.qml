import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import RawVibes

// The file and folder dialogs Rust asks for: native on every platform. Each answers with the command
// the request names, its path(s) and the request's `extra`.
Item {
    id: dl
    property var req: ({})
    function answer(args) {
        args.extra = req.extra || {}
        Store.cmd(req.reply, args)
    }
    function folderUrl(p) { return p ? "file://" + (p.startsWith("/") ? "" : "/") + p : "" }
    Connections {
        target: Backend
        function onDialog(kind, options) {
            dl.req = JSON.parse(options)
            const r = dl.req
            if (kind === "folder") {
                folder.title = r.title || ""
                if (r.folder) folder.currentFolder = dl.folderUrl(r.folder)
                folder.open()
            } else if (kind === "confirm") {
                confirm.title = r.title || ""
                confirmText.text = r.body || ""
                confirmGo.text = r.action || "OK"
                confirm.open()
            } else if (kind === "presetName") {
                name.text = r.name || ""
                ask.open()
            } else {
                file.title = r.title || ""
                file.fileMode = kind === "save" ? FileDialog.SaveFile : kind === "openMany" ? FileDialog.OpenFiles : FileDialog.OpenFile
                file.nameFilters = (r.filters || []).concat([Store.tr("All files") + " (*)"])
                if (r.folder) file.currentFolder = dl.folderUrl(r.folder)
                file.selectedFile = r.name && r.folder ? dl.folderUrl(r.folder + "/" + r.name) : (r.name ? r.name : "")
                file.open()
            }
        }
    }
    Dialog {
        id: confirm
        modal: true
        anchors.centerIn: Overlay.overlay
        background: Rectangle { color: Theme.panel; radius: 12; border.color: Theme.line }
        footer: DialogButtonBox {
            background: null
            Btn { text: Store.tr("Cancel"); DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
            Btn { id: confirmGo; highlighted: true; DialogButtonBox.buttonRole: DialogButtonBox.AcceptRole }
        }
        onAccepted: Store.cmd(dl.req.reply, {})
        Label { id: confirmText; width: 340; wrapMode: Text.Wrap; color: Theme.text }
    }
    FileDialog {
        id: file
        onAccepted: {
            if (fileMode === FileDialog.OpenFiles) dl.answer({ paths: selectedFiles.map(Store.localPath) })
            else dl.answer({ path: Store.localPath(selectedFile) })
        }
    }
    FolderDialog {
        id: folder
        onAccepted: dl.answer({ path: Store.localPath(selectedFolder) })
    }
    Dialog {
        id: ask
        title: Store.tr("Save Preset")
        modal: true
        anchors.centerIn: Overlay.overlay
        standardButtons: Dialog.Save | Dialog.Cancel
        background: Rectangle { color: Theme.panel; radius: 12; border.color: Theme.line }
        onAccepted: if (name.text.trim() !== "") Store.cmd(dl.req.reply, { name: name.text })
        Column {
            spacing: 10
            Label { width: 320; wrapMode: Text.Wrap; color: Theme.dim; text: Store.tr("The grade is saved with all its settings and can be applied to any photo.") }
            TextField { id: name; width: 320; Accessible.name: Store.tr("Preset name"); onAccepted: ask.accept() }
        }
        onOpened: { name.forceActiveFocus(); name.selectAll() }
    }
}
