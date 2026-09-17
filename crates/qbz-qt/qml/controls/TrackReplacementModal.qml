// "Find available version" — the replacement modal for a track Qobuz PULLED
// from the catalogue (2026-08-17 unavailable-tracks contract §6).
//
// It has NO Slint counterpart: the feature never existed there, and the only
// prior art is the Tauri TrackReplacementModal.svelte, which showed Qobuz's raw
// relevance order with no scoring at all. The list here is RANKED by the shared
// weighted matcher (qbz-playlist-import), the head of it is preselected, and an
// ISRC-identical candidate is labelled as the exact relink it is — see
// track_replace_qt.rs for the ordering rules. The human still confirms; nothing
// swaps itself.
//
// Mounted ONCE in AppShell next to PlaylistPickerModal, for the same reason:
// the apply RELOADS the playlist underneath the modal, so a modal parented into
// the playlist view would be torn down by its own success.
//
// Data: QbzTrackReplace.replaceJson (track_replace_qt.rs ReplaceDoc). The
// candidate list is built in Rust — ranking, the ISRC short-circuit, the
// same-id guard and the streamable filter all live there, so this file renders
// rows and owns nothing but the query field's local text.
//
// No `z`: this file's neighbours in AppShell are ordered by DECLARATION, and
// ADR-009's z >= 3000 governs IN-PANE modals. A window-level overlay covering
// the content pane's rounded corners is correct — a window-wide scrim covers
// them too.

// No QtQuick.Controls import: every type here is QtQuick or a project
// component, and nothing on this surface uses the attached ToolTip that pulls
// the module into rows/TrackRow.qml.
import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root
    // An unopened overlay is an invisible, non-interactive Item and costs
    // nothing (the AppShell mount comment states the contract).
    visible: doc.open === true

    readonly property var doc: JSON.parse(QbzTrackReplace.replaceJson)
    readonly property var rows: doc.candidates || []
    readonly property bool loading: doc.loading === true
    readonly property bool applying: doc.applying === true
    readonly property string selectedId: doc.selectedId || ""
    readonly property string selectedAlbumId: doc.selectedAlbumId || ""
    readonly property bool releaseMode: doc.mode === "release"
    readonly property string targetKind: doc.targetKind || "playlist"

    // The ONLY local state in the file: what the user has typed but not yet
    // submitted. Everything else is Rust's document, so a republish (a late
    // search landing, the apply latch) can never fight the view.
    property string queryText: ""

    onVisibleChanged: if (visible) queryText = root.doc.query || ""

    // Re-seed when RUST changes the query (the open path builds "title artist")
    // and the field is not being edited.
    Connections {
        target: QbzTrackReplace
        function onReplaceJsonChanged() {
            if (!queryInput.activeFocus)
                root.queryText = root.doc.query || ""
        }
    }

    function submitQuery() {
        if (root.applying)
            return
        var q = root.queryText.trim()
        if (q !== "")
            QbzTrackReplace.search(q)
    }

    QbzTheme { id: theme }

    // Scrim — click outside dismisses, unless a write is in flight: closing
    // mid-apply would hide the outcome of a playlist mutation the user cannot
    // undo from anywhere else.
    Rectangle {
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: if (!root.applying) QbzTrackReplace.close()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: card
        anchors.centerIn: parent
        width: Math.min(parent.width - 48, 760)
        height: Math.min(parent.height - 48, 520)
        radius: theme.radiusLg
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle
        clip: true

        // Swallow clicks so they never reach the scrim.
        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Column {
            id: panel
            width: parent.width

            // --- Header ---------------------------------------------------
            Item {
                width: parent.width
                height: 72
                Column {
                    anchors.left: parent.left
                    anchors.leftMargin: 24
                    anchors.right: closeX.left
                    anchors.rightMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 3
                    Text {
                        width: parent.width
                        text: root.releaseMode
                            ? QbzSession.tr("Look for better replacement", QbzSession.trRev)
                            : QbzSession.tr("Find available version", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    // WHICH row is being repaired. The modal is opened from a
                    // row context menu and the list below is full of near-
                    // identical titles, so naming the dead track is not
                    // decoration — it is how the user checks they right-clicked
                    // the row they meant.
                    Text {
                        width: parent.width
                        text: (root.doc.deadTitle || "")
                              + ((root.doc.deadArtist || "") !== ""
                                  ? " — " + root.doc.deadArtist : "")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        elide: Text.ElideRight
                    }
                }
                Item {
                    id: closeX
                    anchors.right: parent.right
                    anchors.rightMargin: 24
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 17
                        height: 17
                        tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: closeArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        enabled: !root.applying
                        onClicked: QbzTrackReplace.close()
                    }
                }
            }
            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }

            // --- Query --------------------------------------------------
            // Editable and re-searchable: "title artist" is only a first guess,
            // and the case this feature exists for is precisely the one where
            // the recording moved and the words changed with it.
            Item {
                width: parent.width
                height: 43 + 24
                Rectangle {
                    anchors.left: parent.left
                    anchors.leftMargin: 24
                    anchors.right: parent.right
                    anchors.rightMargin: 24
                    anchors.verticalCenter: parent.verticalCenter
                    height: 43
                    radius: theme.radiusSm
                    color: theme.surfaceCard
                    border.width: 1
                    border.color: queryInput.activeFocus ? theme.accent : theme.borderSubtle

                    TextInput {

                        QbzTextEditMenu { }
                        id: queryInput
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 40
                        color: theme.textPrimary
                        font.pixelSize: theme.fontLink
                        verticalAlignment: Text.AlignVCenter
                        clip: true
                        selectByMouse: true
                        enabled: !root.applying
                        text: root.queryText
                        onTextEdited: root.queryText = text
                        onAccepted: root.submitQuery()
                        // Re-seed while not being edited (the
                        // PlaylistPickerModal Binding shape).
                        Binding {
                            target: queryInput
                            property: "text"
                            value: root.queryText
                            when: !queryInput.activeFocus
                        }
                    }
                    Item {
                        anchors.right: parent.right
                        anchors.rightMargin: 10
                        anchors.verticalCenter: parent.verticalCenter
                        width: 22
                        height: 22
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "search"
                            width: 15
                            height: 15
                            tintName: searchArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: searchArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            enabled: !root.applying
                            onClicked: root.submitQuery()
                        }
                    }
                }
            }

            // --- Candidates ---------------------------------------------
            Item {
                width: parent.width
                // Fills whatever the card has left between the query row and
                // the footer, so the list scrolls instead of the card growing.
                height: Math.max(120, card.height - 72 - 1 - 67 - 100)

                QbzSpinner {
                    anchors.centerIn: parent
                    size: 30
                    visible: root.loading
                }

                // Nothing came back. It is a real outcome, not an error: the
                // recording may simply not be on Qobuz any more in any form.
                Text {
                    anchors.centerIn: parent
                    width: parent.width - 96
                    visible: !root.loading && root.rows.length === 0
                    text: root.releaseMode
                        ? QbzSession.tr("No replacement releases found", QbzSession.trRev)
                        : QbzSession.tr("No available version found", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: theme.fontLink
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                }

                ListView {
                    id: list
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 24
                    anchors.topMargin: 4
                    anchors.bottomMargin: 4
                    visible: !root.loading && root.rows.length > 0
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds
                    model: root.rows
                    spacing: 2

                    delegate: Rectangle {
                        id: cand
                        required property var modelData
                        readonly property bool picked: root.selectedId === modelData.id
                        width: ListView.view ? ListView.view.width : 0
                        height: 64
                        radius: theme.radiusSm
                        color: picked ? theme.surfaceElevated
                             : (candArea.containsMouse ? theme.surfaceHover : "transparent")
                        border.width: picked ? 1 : 0
                        border.color: theme.accent

                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 10
                            anchors.rightMargin: 10
                            spacing: 10

                            // Cover. `artPath` arrives EMPTY on the first
                            // publish and is filled by a second one once the
                            // download settles, so the placeholder is the
                            // normal first frame, not a failure.
                            Rectangle {
                                anchors.verticalCenter: parent.verticalCenter
                                width: 46
                                height: 46
                                radius: 3
                                color: theme.surfaceElevated
                                clip: true
                                Image {
                                    anchors.fill: parent
                                    visible: (cand.modelData.artPath || "") !== ""
                                    source: cand.modelData.artPath || ""
                                    sourceSize.width: 80
                                    sourceSize.height: 80
                                    fillMode: Image.PreserveAspectCrop
                                    asynchronous: true
                                }
                            }

                            Column {
                                anchors.verticalCenter: parent.verticalCenter
                                width: parent.width - 40 - 10 - rightCell.width - 10
                                spacing: 2
                                Row {
                                    width: parent.width
                                    spacing: 6
                                    Text {
                                        width: Math.min(implicitWidth,
                                            parent.width - (exactBadge.visible ? exactBadge.width + 6 : 0)
                                                         - (weakBadge.visible ? weakBadge.width + 6 : 0))
                                        text: cand.modelData.title || ""
                                        color: theme.textPrimary
                                        font.pixelSize: theme.fontLink
                                        elide: Text.ElideRight
                                    }
                                    // THE EXACT RELINK. Same ISRC = the same
                                    // recording under a new catalog id, which
                                    // is the owner's "a veces cambia el ID del
                                    // álbum" case. Saying so is the difference
                                    // between a certainty and a good guess, and
                                    // the user is the one signing off.
                                    Rectangle {
                                        id: exactBadge
                                        anchors.verticalCenter: parent.verticalCenter
                                        visible: cand.modelData.exact === true
                                        width: exactText.implicitWidth + 12
                                        height: 16
                                        radius: 3
                                        color: theme.accent
                                        Text {
                                            id: exactText
                                            anchors.centerIn: parent
                                            text: QbzSession.tr("Exact match", QbzSession.trRev)
                                            color: "#ffffff"
                                            font.pixelSize: 9
                                            font.weight: theme.weightMedium
                                        }
                                    }
                                    // Below the matcher's own confidence floor.
                                    // Offered anyway — a human is confirming,
                                    // and a weak candidate is often the only
                                    // one there is — but never dressed up as a
                                    // match.
                                    Rectangle {
                                        id: weakBadge
                                        anchors.verticalCenter: parent.verticalCenter
                                        visible: cand.modelData.weak === true
                                        width: weakText.implicitWidth + 12
                                        height: 16
                                        radius: 3
                                        color: theme.surfaceElevated
                                        border.width: 1
                                        border.color: theme.borderSubtle
                                        Text {
                                            id: weakText
                                            anchors.centerIn: parent
                                            text: QbzSession.tr("Low confidence", QbzSession.trRev)
                                            color: theme.textMuted
                                            font.pixelSize: 9
                                            font.weight: theme.weightMedium
                                        }
                                    }
                                }
                                Text {
                                    width: parent.width
                                    text: (cand.modelData.artist || "")
                                          + (root.releaseMode
                                              ? (((cand.modelData.year || "") !== "")
                                                  ? "  ·  " + cand.modelData.year : "")
                                              : (((cand.modelData.album || "") !== "")
                                                  ? "  ·  " + cand.modelData.album : ""))
                                    color: theme.textMuted
                                    font.pixelSize: theme.fontLegal
                                    elide: Text.ElideRight
                                }
                            }

                            Row {
                                id: rightCell
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: 8
                                // Quality is a DECISION AID here, not chrome:
                                // the owner's other case is a tier being pulled
                                // and another added, so which tier a candidate
                                // carries is often the whole choice.
                                QualityBadge {
                                    anchors.verticalCenter: parent.verticalCenter
                                    tierOverride: cand.modelData.qualityTier || ""
                                    label: cand.modelData.qualityDetail || ""
                                }
                                // The mark alone says "CD" or "Hi-Res"; the
                                // exact tier is the decision, so it is spelled
                                // out beside it. (The badge's own hover tooltip
                                // never shows on this row: the row's MouseArea
                                // takes the hover.)
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    visible: (cand.modelData.qualityDetail || "") !== ""
                                    text: cand.modelData.qualityDetail || ""
                                    color: theme.textSecondary
                                    font.pixelSize: theme.fontLegal
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: root.releaseMode ? "" : (cand.modelData.duration || "")
                                    color: theme.textMuted
                                    font.pixelSize: theme.fontLegal
                                }
                            }
                        }

                        MouseArea {
                            id: candArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            enabled: !root.applying
                            onClicked: QbzTrackReplace.select(String(cand.modelData.id))
                        }
                    }
                }
            }
        }

        // --- Footer, pinned to the card bottom ---------------------------
        Item {
            id: footer
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            height: 100

            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                height: 1
                color: theme.borderSubtle
            }

            // State the write separately from the action row. This keeps the
            // disclosure readable after the shared third button is added.
            Text {
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.top: parent.top
                anchors.topMargin: 9
                visible: root.rows.length > 0
                text: root.releaseMode
                    ? (root.targetKind === "album"
                        ? QbzSession.tr("The selected release replaces the unavailable album favorite.",
                                        QbzSession.trRev)
                        : QbzSession.tr("The matching track from the selected release replaces the unavailable favorite.",
                                        QbzSession.trRev))
                    : QbzSession.tr("The replacement takes the unavailable track's place in the playlist.",
                                    QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                elide: Text.ElideRight
            }

            Row {
                id: footerRow
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.bottom: parent.bottom
                anchors.bottomMargin: 14
                spacing: 10

                SettingsButton {
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Cancel", QbzSession.trRev)
                    btnHeight: 34
                    minWidth: 0
                    enabled: !root.applying
                    onClicked: QbzTrackReplace.close()
                }
                SettingsButton {
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Open album", QbzSession.trRev)
                    btnHeight: 34
                    minWidth: 0
                    enabled: !root.applying && !root.loading
                        && root.selectedAlbumId !== ""
                    onClicked: QbzTrackReplace.openSelectedAlbum()
                }
                // The single accent mutation (ADR-008). Open album remains a
                // neutral inspection action and never writes.
                QbzPrimaryButton {
                    anchors.verticalCenter: parent.verticalCenter
                    btnHeight: 34
                    label: root.releaseMode
                        ? (root.applying
                            ? QbzSession.tr("Replacing favorite...", QbzSession.trRev)
                            : QbzSession.tr("Replace in Library", QbzSession.trRev))
                        : (root.applying
                        ? QbzSession.tr("Replacing...", QbzSession.trRev)
                        : QbzSession.tr("Replace", QbzSession.trRev))
                    btnEnabled: !root.applying && !root.loading && root.selectedId !== ""
                    onClicked: QbzTrackReplace.apply()
                }
            }
        }
    }
}
