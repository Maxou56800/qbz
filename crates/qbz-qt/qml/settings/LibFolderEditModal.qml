// Per-folder settings for a registered Local Library folder — QML port of
// crates/qbz-ui/ui/settings/LibFolderEditModal.slint (itself the Tauri
// FolderSettingsModal). Alias, enabled, network override + fs-type,
// accessibility status, change-path, last-scanned, scan-this-folder.
//
// THE FIELD THAT MATTERS: `userOverrideNetwork`. It has a real consumer in
// the SHARED scanner (`qbz-library/src/scan.rs:164`), where it suppresses
// per-scan network re-detection. Until this modal existed the Qt build had no
// writer for it, so a user who classified a folder by hand had that decision
// silently overwritten by detection on the next scan. This is not a cosmetic
// port.
//
// State rides the settings document (`doc.library.folderEdit`) rather than a
// bridge of its own — it is a surface of the Local Library panel and all its
// actions already land in settings_qt/library.rs. Draft edits are LOCAL to
// this file until Save, so Cancel really cancels; only the accessibility
// probe and the path change write back before then (both come FROM Rust).
//
// MOUNTED AT THE SETTINGS VIEW ROOT, beside SettingsConfirmHost — the panels
// are Columns inside a Flickable, so a modal mounted in one would be sized by
// the scrolled content and ride the scroll. ADR-009: z >= 3000.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Item {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    readonly property var st: (doc.library && doc.library.folderEdit) || ({})

    anchors.fill: parent
    z: 3100
    visible: root.st.open === true
    enabled: root.visible

    QbzTheme { id: theme }

    // The option list and its stored labels, in the reference's order
    // (LibFolderEditModal.slint:266-278 / :37-48). Index 0 = auto-detect.
    readonly property var fsTypeValues: [
        "auto", "cifs", "nfs", "sshfs", "rclone", "webdav", "glusterfs", "ceph", "other"
    ]

    // --- Draft state ----------------------------------------------------
    // Seeded on every open, never bound: a binding onto the document would
    // snap the user's half-typed alias back on the next publish (the probe
    // alone republishes twice per open).
    property string draftAlias: ""
    property bool draftEnabled: true
    property bool draftIsNetwork: false
    property bool draftOverride: false
    property int draftFsIndex: 0

    onVisibleChanged: {
        if (!visible)
            return
        root.draftAlias = root.st.alias || ""
        root.draftEnabled = root.st.enabled === true
        root.draftIsNetwork = root.st.isNetwork === true
        root.draftOverride = root.st.userOverrideNetwork === true
        root.draftFsIndex = root.st.fsTypeIndex || 0
        aliasField.reset(root.draftAlias)
        closeButton.forceActiveFocus()
    }

    function close() {
        QbzBridge.settingsString("library-folder-edit-close", "")
    }

    function save() {
        QbzBridge.settingsString("library-folder-edit-save", JSON.stringify({
            id: root.st.folderId || 0,
            alias: root.draftAlias,
            enabled: root.draftEnabled,
            isNetwork: root.draftIsNetwork,
            fsType: root.fsTypeValues[root.draftFsIndex] || "auto",
            userOverrideNetwork: root.draftOverride
        }))
    }

    Keys.onEscapePressed: function (event) {
        root.close()
        event.accepted = true
    }

    // Scrim — click dismisses.
    Rectangle {
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: root.close()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    // Faked 32px drop shadow (#00000080) — the LoginScreen/QbzConfirmModal
    // precedent; a real blurred shadow owes its own parity pass.
    Rectangle {
        anchors.centerIn: panel
        width: panel.width + 8
        height: panel.height + 8
        radius: theme.radiusMd
        color: "#80000000"
        z: -1
    }

    Rectangle {
        id: panel
        anchors.centerIn: parent
        width: Math.min(root.width - 80, 520)
        height: Math.min(root.height - 80, body.implicitHeight + 44)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle

        // Swallow clicks so they never reach the scrim.
        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Flickable {
            anchors.fill: parent
            clip: root.kioskHost
            interactive: root.kioskHost
            contentWidth: width
            contentHeight: body.implicitHeight + 44
            boundsBehavior: Flickable.StopAtBounds
        Column {
            id: body
            x: 22
            y: 22
            width: parent.width - 44
            spacing: 16

            // --- Title + close ------------------------------------------
            Item {
                width: parent.width
                height: root.kioskHost ? 44 : 28
                Text {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Folder settings", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost ? (theme.fontHeading) * 1.2 : (theme.fontHeading)
                    font.weight: theme.weightSemibold
                }
                Rectangle {
                    id: closeButton
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: root.kioskHost ? 44 : 28
                    height: root.kioskHost ? 44 : 28
                    radius: theme.radiusSm
                    color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                    activeFocusOnTab: root.visible
                    border.width: activeFocus ? 2 : 0
                    border.color: theme.accent
                    Accessible.role: Accessible.Button
                    Accessible.name: QbzSession.tr("Close", QbzSession.trRev)
                    Accessible.onPressAction: root.close()
                    Keys.onPressed: function (event) {
                        if (!event.isAutoRepeat
                                && (event.key === Qt.Key_Space
                                    || event.key === Qt.Key_Return
                                    || event.key === Qt.Key_Enter)) {
                            root.close()
                            event.accepted = true
                        }
                    }
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 17
                        height: 17
                        tintName: closeArea.containsMouse ? "primary" : "muted"
                    }
                    MouseArea {
                        id: closeArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onPressed: closeButton.forceActiveFocus()
                        onClicked: root.close()
                    }
                }
            }

            // --- Folder location + Change + accessibility ---------------
            Column {
                width: parent.width
                spacing: 6
                Text {
                    text: QbzSession.tr("Folder location", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                    font.weight: theme.weightSemibold
                }
                Item {
                    width: parent.width
                    height: 32
                    QbzIcon {
                        id: locIcon
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        name: root.draftIsNetwork ? "network" : "hard-drive"
                        width: 18
                        height: 18
                        tintName: root.draftIsNetwork ? "accent" : "secondary"
                    }
                    Text {
                        anchors.left: locIcon.right
                        anchors.leftMargin: 10
                        anchors.right: changeBtn.left
                        anchors.rightMargin: 10
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.st.path || ""
                        color: theme.textSecondary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                        elide: Text.ElideMiddle
                    }
                    SettingsButton { kioskHost: root.kioskHost;
                        id: changeBtn
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        iconName: "folder-open"
                        text: QbzSession.tr("Change", QbzSession.trRev)
                        onClicked: QbzBridge.settingsString(
                            "library-folder-change-path", String(root.st.folderId || 0))
                    }
                }
                // Accessibility status. `checkingAccessible` is true while the
                // Rust probe is out — it starts that way on every open, so a
                // dead mount does not hold the modal closed.
                Row {
                    spacing: 8
                    QbzIcon {
                        visible: root.st.checkingAccessible !== true
                        anchors.verticalCenter: parent.verticalCenter
                        name: root.st.accessible === true ? "check" : "x"
                        width: 14
                        height: 14
                        // The reference hard-codes #3fae6a / #e0564f. QbzIcon
                        // tints from a baked SET, not an arbitrary colour, so
                        // these go through the port's two documented
                        // substitutions: "green" (#22c55e) and "favorite"
                        // (#ef4444, which icon_tint_qt.rs:63 already records
                        // as the port-wide stand-in for #e0564f danger).
                        tintName: root.st.accessible === true ? "green" : "favorite"
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.st.checkingAccessible === true
                            ? QbzSession.tr("Checking...", QbzSession.trRev)
                            : (root.st.accessible === true
                                ? QbzSession.tr("Folder is accessible", QbzSession.trRev)
                                : QbzSession.tr("Folder is not accessible", QbzSession.trRev))
                        color: theme.textMuted
                        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                    }
                }
            }

            // --- Display name -------------------------------------------
            Column {
                width: parent.width
                spacing: 6
                Text {
                    text: QbzSession.tr("Display name", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                    font.weight: theme.weightSemibold
                }
                QbzLineEdit { kioskHost: root.kioskHost;
                    id: aliasField
                    width: parent.width
                    placeholder: QbzSession.tr("Optional friendly name", QbzSession.trRev)
                    onEdited: function (v) { root.draftAlias = v }
                    onCommitted: function (v) { root.draftAlias = v }
                }
                Text {
                    text: QbzSession.tr("Shown instead of the full path in the folder list.", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                }
            }

            // --- Enabled -------------------------------------------------
            Item {
                width: parent.width
                height: 40
                Column {
                    anchors.left: parent.left
                    anchors.right: enabledToggle.left
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2
                    Text {
                        text: QbzSession.tr("Enabled", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                    }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("Disabled folders are skipped during scans.", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                        wrapMode: Text.WordWrap
                    }
                }
                QbzToggle { kioskHost: root.kioskHost;
                    id: enabledToggle
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    checked: root.draftEnabled
                    onToggled: function (v) { root.draftEnabled = v }
                }
            }

            // --- Network override ----------------------------------------
            // Flipping this sets userOverrideNetwork, which is what tells the
            // shared scanner to stop re-detecting (scan.rs:164).
            Item {
                width: parent.width
                height: 40
                Column {
                    anchors.left: parent.left
                    anchors.right: netToggle.left
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2
                    Text {
                        text: QbzSession.tr("Network folder", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                    }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("Mark this folder as a network share (excluded when offline).", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                        wrapMode: Text.WordWrap
                    }
                }
                QbzToggle { kioskHost: root.kioskHost;
                    id: netToggle
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    checked: root.draftIsNetwork
                    onToggled: function (v) {
                        root.draftIsNetwork = v
                        root.draftOverride = true
                    }
                }
            }

            // --- FS type (network only) ----------------------------------
            Column {
                width: parent.width
                spacing: 6
                visible: root.draftIsNetwork
                Text {
                    text: QbzSession.tr("Network filesystem type", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                    font.weight: theme.weightSemibold
                }
                QbzSelect { kioskHost: root.kioskHost;
                    menuWidth: 200
                    options: [
                        QbzSession.tr("Auto-detect", QbzSession.trRev),
                        "CIFS / SMB", "NFS", "SSHFS", "rclone", "WebDAV",
                        "GlusterFS", "Ceph",
                        QbzSession.tr("Other", QbzSession.trRev)
                    ]
                    currentIndex: root.draftFsIndex
                    onSelected: function (i) { root.draftFsIndex = i }
                }
            }

            // --- Last scanned --------------------------------------------
            Item {
                width: parent.width
                height: 18
                Text {
                    anchors.left: parent.left
                    text: QbzSession.tr("Last scanned", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                }
                Text {
                    anchors.right: parent.right
                    text: (root.st.lastScan || 0) > 0
                        ? new Date((root.st.lastScan || 0) * 1000).toLocaleString(Qt.locale())
                        : QbzSession.tr("Never", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                }
            }

            Rectangle {
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }

            // --- Footer ---------------------------------------------------
            Item {
                width: parent.width
                height: 36
                // Scan needs the folder to be both enabled AND reachable —
                // scanning a disabled or dead folder is a guaranteed no-op.
                SettingsButton { kioskHost: root.kioskHost;
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    iconName: "refresh-cw"
                    text: QbzSession.tr("Scan this folder", QbzSession.trRev)
                    enabled: root.draftEnabled && root.st.accessible === true
                    onClicked: {
                        QbzBridge.settingsString("library-scan", String(root.st.folderId || 0))
                        root.close()
                    }
                }
                Row {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 10
                    SettingsButton { kioskHost: root.kioskHost;
                        text: QbzSession.tr("Cancel", QbzSession.trRev)
                        onClicked: root.close()
                    }
                    // Accent-FILLED primary. Built inline rather than by
                    // adding an `accent` arm to the shared SettingsButton:
                    // that control is outlined-only everywhere it is used
                    // today, and QbzConfirmModal already draws its primary
                    // this same way (:218-232), including the
                    // `accentGlyphColor` selector for the label.
                    Rectangle {
                        id: saveButton
                        anchors.verticalCenter: parent.verticalCenter
                        width: saveLabel.implicitWidth + 36
                        height: 36
                        radius: theme.radiusSm
                        color: saveArea.containsMouse ? theme.accentHover : theme.accent
                        activeFocusOnTab: root.visible
                        border.width: activeFocus ? 2 : 0
                        border.color: theme.accentGlyphColor
                        Accessible.role: Accessible.Button
                        Accessible.name: QbzSession.tr("Save", QbzSession.trRev)
                        Accessible.onPressAction: root.save()
                        Keys.onPressed: function (event) {
                            if (!event.isAutoRepeat
                                    && (event.key === Qt.Key_Space
                                        || event.key === Qt.Key_Return
                                        || event.key === Qt.Key_Enter)) {
                                root.save()
                                event.accepted = true
                            }
                        }
                        Text {
                            id: saveLabel
                            anchors.centerIn: parent
                            text: QbzSession.tr("Save", QbzSession.trRev)
                            color: theme.accentGlyphColor
                            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            font.weight: theme.weightSemibold
                        }
                        MouseArea {
                            id: saveArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onPressed: saveButton.forceActiveFocus()
                            onClicked: root.save()
                        }
                    }
                }
            }
        }
        }
    }
}
