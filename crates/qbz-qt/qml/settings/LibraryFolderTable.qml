// Settings > Local Library > LIBRARY FOLDERS — the folder-management block of
// crates/qbz-ui/ui/settings/LocalLibrarySettings.slint (lines 256-421): the
// toolbar, the filter, the compact folder table (checkbox · folder · last
// scan · status · actions) and the determinate scan progress with its Stop.
//
// Split out of LocalLibrarySettings.qml so that panel stays a thin section
// composition. It owns its own pure-UI state (filter + selection), exactly
// like the Slint keeps it in LibraryFoldersState; everything else rides the
// settings document and the settingsString action keys
// (settings_qt/library.rs).

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    /// The `library` sub-document of the settings snapshot.
    property var lib: ({})
    readonly property var folders: lib.folders || []

    // Pure-UI state (the Slint keeps the same in LibraryFoldersState).
    property string filter: ""
    property string pendingPath: ""
    property var selectedIds: []

    QbzTheme { id: theme }

    spacing: 4

    function shown() {
        const q = filter.trim().toLowerCase()
        if (q === "") return folders
        return folders.filter(function (f) {
            return (f.displayName || "").toLowerCase().indexOf(q) >= 0
                || (f.path || "").toLowerCase().indexOf(q) >= 0
        })
    }
    function isSelected(id) { return selectedIds.indexOf(id) >= 0 }
    /// A plain or Ctrl click toggles one folder; Shift adds the range from the
    /// last plain click over the rows SHOWN (the filter is a view filter) —
    /// the rule every multi-select list shares (controls/SelectionModel.qml).
    SelectionModel { id: folderSel }
    function toggleSelected(id, mods) {
        const rows = root.shown()
        const current = {}
        for (let i = 0; i < selectedIds.length; i++)
            current[String(selectedIds[i])] = true
        const picked = folderSel.next(current, id, rows,
                                      mods === undefined ? Qt.NoModifier : mods)
        // Back to the ids themselves (the removal action takes them as
        // numbers), keeping earlier picks the filter currently hides.
        const next = selectedIds.filter(function (sid) { return picked[String(sid)] === true })
        for (let j = 0; j < rows.length; j++)
            if (picked[String(rows[j].id)] === true && next.indexOf(rows[j].id) < 0)
                next.push(rows[j].id)
        selectedIds = next
    }
    function scanLabel(ts) {
        if (!ts) return QbzSession.tr("Never", QbzSession.trRev)
        return Qt.formatDateTime(new Date(ts * 1000), "yyyy-MM-dd hh:mm")
    }
    readonly property var statusLabels: ({
        active: QbzSession.tr("Active", QbzSession.trRev),
        hidden: QbzSession.tr("Hidden", QbzSession.trRev),
        missing: QbzSession.tr("Missing", QbzSession.trRev),
        disconnected: QbzSession.tr("Disconnected", QbzSession.trRev),
        denied: QbzSession.tr("Access denied", QbzSession.trRev),
        unavailable: QbzSession.tr("Unavailable", QbzSession.trRev),
        checking: QbzSession.tr("Checking...", QbzSession.trRev)
    })
    readonly property var statusDescriptions: ({
        active: QbzSession.tr("The folder is accessible and visible in the library.", QbzSession.trRev),
        hidden: QbzSession.tr("The folder is accessible but hidden by your preference.", QbzSession.trRev),
        missing: QbzSession.tr("The folder path no longer exists. Reconnect it, change its path or remove it from the library.", QbzSession.trRev),
        disconnected: QbzSession.tr("A connection failure prevents access to this storage.", QbzSession.trRev),
        denied: QbzSession.tr("QBZ does not have permission to access this folder.", QbzSession.trRev),
        unavailable: QbzSession.tr("The folder could not be accessed. The check timed out or returned another access error.", QbzSession.trRev),
        checking: QbzSession.tr("Folder access is being checked or has not been checked yet.", QbzSession.trRev)
    })
    FontMetrics {
        id: statusMetrics
        font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
        font.weight: theme.weightMedium
    }
    readonly property int statusW: Math.ceil(Math.max(110,
        ...Object.values(root.statusLabels).map(function(s) { return statusMetrics.advanceWidth(s) + 20 })))
    readonly property int actionButtonW: root.kioskHost ? 44 : 40
    readonly property int actionsW: 4 * actionButtonW + 3 * 4
    function folderColW(rowWidth) {
        return Math.max(100, rowWidth - (root.kioskHost ? 44 : 20) - 120 - statusW - actionsW - 4 * 12)
    }
    property string lastPickedPath: ""
    onLibChanged: {
        const picked = root.lib.picked_path || ""
        if (picked !== lastPickedPath) {
            lastPickedPath = picked
            if (picked !== "") { root.pendingPath = picked; pathField.text = picked }
        }
    }

    Item {
        width: parent.width
        height: root.kioskHost ? 44 : 34
        GroupHeader { kioskHost: root.kioskHost;
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            text: QbzSession.tr("LIBRARY FOLDERS", QbzSession.trRev)
        }
        Row {
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            spacing: 8
            // Scan every enabled folder.
            SettingsButton { kioskHost: root.kioskHost;
                iconName: "refresh-cw"
                enabled: root.lib.scanning !== true
                onClicked: QbzBridge.settingsString("library-scan", "")
            }
            // Remove the selected folders (their indexed tracks go with them).
            SettingsButton { kioskHost: root.kioskHost;
                iconName: "trash-2"
                danger: true
                enabled: root.selectedIds.length > 0
                onClicked: {
                    QbzBridge.settingsString("library-remove-folders",
                        JSON.stringify(root.selectedIds))
                    root.selectedIds = []
                    folderSel.anchorId = ""
                }
            }
        }
    }

    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Add folder", QbzSession.trRev)
        description: QbzSession.tr("Browse for a music folder, or type the full path.", QbzSession.trRev)
        Row {
            spacing: 8
            QbzLineEdit { kioskHost: root.kioskHost;
                id: pathField
                width: 190
                anchors.verticalCenter: parent.verticalCenter
                placeholder: "/home/you/Music"
                onEdited: function (v) { root.pendingPath = v }
                onCommitted: function (v) { root.pendingPath = v }
            }
            // Browse fills the same path field as typing; Add is the single commit.
            SettingsButton { kioskHost: root.kioskHost;
                iconName: "folder"
                text: QbzSession.tr("Browse...", QbzSession.trRev)
                onClicked: QbzBridge.settingsString("library-pick-folder", "")
            }
            SettingsButton { kioskHost: root.kioskHost;
                iconName: "folder-plus"
                text: QbzSession.tr("Add", QbzSession.trRev)
                enabled: root.pendingPath !== ""
                onClicked: {
                    QbzBridge.settingsString("library-add-folder", root.pendingPath)
                    root.pendingPath = ""
                    pathField.text = ""
                }
            }
        }
    }
    Text {
        visible: (root.lib.status || "") !== ""
        width: parent.width
        text: root.lib.status || ""
        color: theme.danger
        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
        wrapMode: Text.WordWrap
    }

    // Filter.
    Item {
        width: parent.width
        height: root.kioskHost ? 44 : 38
        QbzLineEdit { kioskHost: root.kioskHost;
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            width: 220
            searchMode: true
            sm: true
            placeholder: QbzSession.tr("Filter folders...", QbzSession.trRev)
            onEdited: function (q) { root.filter = q }
        }
    }

    // Empty state.
    Text {
        visible: root.folders.length === 0
        width: parent.width
        text: root.filter === ""
            ? QbzSession.tr("No folders yet. Add a folder to build your local library.", QbzSession.trRev)
            : QbzSession.tr("No folders match your filter.", QbzSession.trRev)
        color: theme.textMuted
        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
        wrapMode: Text.WordWrap
    }

    // Keep every column reachable on narrow windows; never paint actions over status.
    Flickable {
        id: folderTable
        width: root.width
        height: tableRows.height
        contentWidth: Math.max(width, 100 + (root.kioskHost ? 44 : 20) + 120 + root.statusW + root.actionsW + 70)
        contentHeight: height
        clip: true
        flickableDirection: Flickable.HorizontalFlick
        boundsBehavior: Flickable.StopAtBounds
        Column {
            id: tableRows
            width: folderTable.contentWidth
            spacing: 4
    // Table header (columns MUST match the rows below).
    Item {
        visible: root.folders.length > 0
        width: parent.width
        height: 26
        Row {
            anchors.fill: parent
            anchors.leftMargin: 10
            anchors.rightMargin: 12
            spacing: 12
            Item { width: root.kioskHost ? 44 : 20; height: 1 }
            Text {
                width: root.folderColW(parent.width)
                height: parent.height
                text: QbzSession.tr("FOLDER", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                font.letterSpacing: 0.5
                verticalAlignment: Text.AlignVCenter
            }
            Text {
                width: 120
                height: parent.height
                text: QbzSession.tr("LAST SCAN", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                font.letterSpacing: 0.5
                verticalAlignment: Text.AlignVCenter
            }
            Text {
                width: root.actionsW
                height: parent.height
                text: QbzSession.tr("Actions", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                verticalAlignment: Text.AlignVCenter
            }
            Text {
                width: root.statusW
                height: parent.height
                text: QbzSession.tr("STATUS", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                font.letterSpacing: 0.5
                verticalAlignment: Text.AlignVCenter
            }

        }
    }
    Rectangle {
        visible: root.folders.length > 0
        width: parent.width
        height: 1
        color: theme.borderSubtle
    }

    Repeater {
        model: root.shown()
        delegate: Rectangle {
            id: folderRow
            required property var modelData

            width: folderTable.contentWidth
            height: root.kioskHost ? 64 : 40
            radius: theme.radiusSm
            color: root.isSelected(modelData.id) ? theme.surfaceElevated
                : rowArea.containsMouse ? theme.surfaceHover : "transparent"

            // Declared FIRST so the per-row buttons (declared after) take
            // their own clicks; the empty row area falls through to select.
            MouseArea {
                id: rowArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: function (mouse) {
                    root.toggleSelected(folderRow.modelData.id, mouse.modifiers)
                }
            }

            Row {
                anchors.fill: parent
                anchors.leftMargin: 10
                anchors.rightMargin: 12
                spacing: 12

                QbzCheckbox { kioskHost: root.kioskHost;
                    width: root.kioskHost ? 44 : 20
                    anchors.verticalCenter: parent.verticalCenter
                    checked: root.isSelected(folderRow.modelData.id)
                    onToggled: function (mods) {
                        root.toggleSelected(folderRow.modelData.id, mods)
                    }
                }
                // FOLDER: type glyph + name.
                Row {
                    width: root.folderColW(parent.width)
                    height: parent.height
                    spacing: 8
                    QbzIcon {
                        anchors.verticalCenter: parent.verticalCenter
                        objectName: "folderType-" + folderRow.modelData.id
                        name: folderRow.modelData.isNetwork ? "server" : "hard-drive"
                        ToolTip.visible: typeHover.containsMouse
                        ToolTip.delay: 400
                        ToolTip.text: folderRow.modelData.isNetwork
                            ? QbzSession.tr("Network folder", QbzSession.trRev)
                            : QbzSession.tr("Local folder", QbzSession.trRev)
                        MouseArea {
                            id: typeHover
                            anchors.fill: parent
                            hoverEnabled: true
                            acceptedButtons: Qt.NoButton
                        }
                        width: 16
                        height: 16
                        tintName: folderRow.modelData.isNetwork ? "accent" : "secondary"
                    }
                    Text {
                        width: parent.width - 24
                        height: parent.height
                        text: folderRow.modelData.displayName || folderRow.modelData.path
                        color: folderRow.modelData.enabled ? theme.textPrimary : theme.textMuted
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                Text {
                    width: 120
                    height: parent.height
                    text: root.scanLabel(folderRow.modelData.lastScan)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                // ACTIONS: settings · enable/disable · scan this folder ·
                // remove it. The eye button duplicates the edit modal's
                // "Enabled" toggle on purpose — it predates the modal and is
                // one click instead of three for the thing people do most.
                Row {
                    objectName: "folderActions-" + folderRow.modelData.id
                    width: root.actionsW
                    height: parent.height
                    spacing: 4
                    layoutDirection: Qt.RightToLeft
                    SettingsButton { kioskHost: root.kioskHost;
                        width: root.actionButtonW
                        anchors.verticalCenter: parent.verticalCenter
                        iconName: "trash-2"
                        onClicked: QbzBridge.settingsString("library-remove-folders",
                            JSON.stringify([folderRow.modelData.id]))
                    }
                    SettingsButton { kioskHost: root.kioskHost;
                        width: root.actionButtonW
                        anchors.verticalCenter: parent.verticalCenter
                        iconName: "refresh-cw"
                        enabled: folderRow.modelData.enabled && root.lib.scanning !== true
                        onClicked: QbzBridge.settingsString("library-scan",
                            String(folderRow.modelData.id))
                    }
                    SettingsButton { kioskHost: root.kioskHost;
                        width: root.actionButtonW
                        anchors.verticalCenter: parent.verticalCenter
                        iconName: folderRow.modelData.enabled ? "eye" : "eye-off"
                        onClicked: QbzBridge.settingsString("library-folder-enabled",
                            String(folderRow.modelData.id))
                    }
                    // The per-folder settings modal (alias, network override
                    // + fs type, change path). The reference reaches it from
                    // the same place; this port had no affordance at all
                    // until the modal landed.
                    SettingsButton { kioskHost: root.kioskHost;
                        width: root.actionButtonW
                        anchors.verticalCenter: parent.verticalCenter
                        iconName: "settings-2"
                        onClicked: QbzBridge.settingsString("library-folder-edit-open",
                            String(folderRow.modelData.id))
                    }
                }
                Text {
                    objectName: "folderStatus-" + folderRow.modelData.id
                    width: root.statusW
                    height: parent.height
                    text: root.statusLabels[folderRow.modelData.status || "checking"] || root.statusLabels.unavailable
                    color: ["missing", "disconnected", "denied", "unavailable"].indexOf(folderRow.modelData.status) >= 0
                        ? theme.danger : theme.textMuted
                    font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                    font.weight: theme.weightMedium
                    verticalAlignment: Text.AlignVCenter
                    ToolTip.visible: statusHover.containsMouse
                    ToolTip.delay: 400
                    ToolTip.text: root.statusDescriptions[folderRow.modelData.status || "checking"] || root.statusDescriptions.unavailable
                    MouseArea {
                        id: statusHover
                        anchors.fill: parent
                        hoverEnabled: true
                        acceptedButtons: Qt.NoButton
                    }
                }

            }
        }
    }

        }
    }

    // --------------------------- scan progress ---------------------------
    Item {
        visible: root.lib.scanning === true
        width: parent.width
        height: visible ? (root.kioskHost ? 90 : 76) : 0
        Column {
            anchors.fill: parent
            anchors.topMargin: 14
            spacing: 8
            Item {
                width: parent.width
                height: root.kioskHost ? 44 : 30
                Text {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Scanning", QbzSession.trRev) + ": "
                        + (root.lib.processed || 0) + " / " + (root.lib.total || 0)
                    color: theme.textSecondary
                    font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                    font.weight: theme.weightMedium
                }
                SettingsButton { kioskHost: root.kioskHost;
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    danger: true
                    iconName: "x"
                    text: QbzSession.tr("Stop", QbzSession.trRev)
                    onClicked: QbzBridge.settingsString("library-scan-stop", "")
                }
            }
            Rectangle {
                width: parent.width
                height: 6
                radius: 3
                color: theme.surfaceElevated
                clip: true
                Rectangle {
                    width: parent.width * ((root.lib.total || 0) > 0
                        ? Math.min(1, (root.lib.processed || 0) / root.lib.total) : 0)
                    height: parent.height
                    radius: 3
                    color: theme.accent
                }
            }
            Text {
                visible: (root.lib.file || "") !== ""
                width: parent.width
                text: root.lib.file || ""
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                elide: Text.ElideRight
            }
        }
    }
}
