// QueueView — a chronological, virtualized projection of playback history,
// the current track and the complete upcoming sequence. The core queue keeps
// its streaming semantics; history becomes playable again only when the user
// activates or drags one of its rows back below Now Playing.

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../rows"
import "../theme"
import "../assets/kiosk-art.js" as KioskArt

Rectangle {
    id: root
    property bool kioskHost: false

    QbzTheme { id: theme }

    color: theme.ambientOn ? "transparent" : theme.surfaceMain
    radius: theme.radiusMd

    readonly property var doc: {
        try {
            return JSON.parse(QbzQueue.extendedQueueJson)
        } catch (e) {
            return ({ "rows": [], "currentIndex": -1, "historyCount": 0,
                      "upcomingCount": 0, "stopAfterId": "",
                      "infinitePlay": false, "searchQuery": "" })
        }
    }
    readonly property var rows: root.doc.rows || []
    readonly property bool searchActive: (root.doc.searchQuery || "") !== ""
    readonly property int firstUpcoming: (root.doc.historyCount || 0)
        + (root.doc.currentIndex >= 0 ? 1 : 0)
    readonly property var currentRow: root.doc.currentIndex >= 0
        ? root.rows[root.doc.currentIndex] : null
    readonly property bool hasPlayingRow: root.doc.currentIndex >= 0
        && root.doc.currentIndex < root.rows.length
    readonly property int actionRailWidth: kioskHost ? 72 : 52

    // Timing for the chronological projection currently on screen. History is
    // fully elapsed, the current row contributes the player's live position,
    // and upcoming rows are fully remaining. A filtered listen list therefore
    // reports the duration of exactly the rows its count describes.
    readonly property var queueTiming: {
        var total = 0
        var elapsed = 0
        for (var i = 0; i < root.rows.length; i++) {
            var row = root.rows[i] || ({})
            var secs = Math.max(0, Math.floor(row.durationSecs || 0))
            total += secs
            if (row.phase === "history")
                elapsed += secs
            else if (row.phase === "current")
                elapsed += Math.min(secs, Math.max(0, QbzPlayer.npElapsedSecs))
        }
        return ({ "total": total, "elapsed": Math.min(total, elapsed),
                  "remaining": Math.max(0, total - elapsed) })
    }

    function compactDuration(secs) {
        var value = Math.max(0, Math.floor(secs || 0))
        var hours = Math.floor(value / 3600)
        var minutes = Math.floor((value % 3600) / 60)
        if (hours > 0)
            return hours + "h " + minutes + "m"
        return minutes + "m"
    }

    function clockDuration(secs) {
        var value = Math.max(0, Math.floor(secs || 0))
        var hours = Math.floor(value / 3600)
        var minutes = Math.floor((value % 3600) / 60)
        var seconds = value % 60
        var mm = (hours > 0 && minutes < 10 ? "0" : "") + minutes
        var ss = (seconds < 10 ? "0" : "") + seconds
        return hours > 0 ? hours + ":" + mm + ":" + ss : minutes + ":" + ss
    }

    readonly property string queueTimingTooltip:
        QbzSession.tr("Elapsed: {}", QbzSession.trRev)
            .replace("{}", root.clockDuration(root.queueTiming.elapsed))
        + "\n"
        + QbzSession.tr("Remaining: {}", QbzSession.trRev)
            .replace("{}", root.clockDuration(root.queueTiming.remaining))
    onQueueTimingTooltipChanged: {
        if (queueSummaryHover.containsMouse)
            tips.showAbove(queueSummary, "queue-timing", root.queueTimingTooltip)
    }

    // Opt-in follow mode. It survives playback-driven document updates but
    // yields immediately to any user scroll or queue-ordering gesture, so a
    // track transition can never yank the viewport away while the user is
    // arranging another part of the list.
    property bool followPlaying: false
    property bool programmaticListPosition: false

    function cancelPlayingFollow() {
        root.followPlaying = false
    }

    function positionListAt(index, mode) {
        if (index < 0 || index >= root.rows.length)
            return
        root.programmaticListPosition = true
        queueList.positionViewAtIndex(index, mode)
        Qt.callLater(function () { root.programmaticListPosition = false })
    }

    function focusOnPlaying() {
        if (!root.hasPlayingRow)
            return
        root.followPlaying = true
        root.positionListAt(root.doc.currentIndex, ListView.Center)
    }

    function followPlayingRow() {
        if (root.followPlaying && root.hasPlayingRow)
            root.positionListAt(root.doc.currentIndex, ListView.Center)
    }

    function goToTop() {
        root.cancelPlayingFollow()
        if (root.rows.length > 0)
            root.positionListAt(0, ListView.Beginning)
    }

    Component.onCompleted: {
        QbzQueue.queueExtendedOpened()
        root.scheduleVisibleCovers()
    }
    Component.onDestruction: {
        QbzShell.dragInlineVisual = false
        QbzQueue.queueExtendedClosed()
    }
    onDocChanged: {
        root.scheduleVisibleCovers()
        if (!root.hasPlayingRow)
            root.cancelPlayingFollow()
        else if (root.followPlaying)
            Qt.callLater(function () { root.followPlayingRow() })
    }
    onSearchActiveChanged: if (root.searchActive) root.cancelPlayingFollow()

    // ----------------------------- artwork ------------------------------

    property var coverMap: ({})
    // URLs in the current visible artwork window. Unlike the old
    // `askedCovers`, this is not a permanent latch: a first miss against a
    // waking NAS must be allowed to settle on a later bounded attempt.
    property var wantedCovers: ({})
    property int artRetriesLeft: 0

    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            if (root.wantedCovers[key] !== true)
                return
            var next = Object.assign({}, root.coverMap)
            next[key] = path
            root.coverMap = next
        }
    }

    function scheduleVisibleCovers() {
        root.artRetriesLeft = 2
        artDispatch.restart()
    }

    function requestVisibleCovers() {
        if (root.rows.length === 0 || queueList.height <= 0)
            return
        var first = queueList.indexAt(4, queueList.contentY + 2)
        if (first < 0)
            first = 0
        var last = queueList.indexAt(4, queueList.contentY + queueList.height - 2)
        if (last < first)
            last = Math.min(root.rows.length - 1, first + 16)
        first = Math.max(0, first - 3)
        last = Math.min(root.rows.length - 1, last + 3)
        var urls = []
        var wanted = ({})
        for (var i = first; i <= last; i++) {
            var url = root.artUrl(root.rows[i])
            if (url !== "" && !root.coverMap[url] && wanted[url] !== true) {
                wanted[url] = true
                urls.push(url)
            }
        }
        // Assign before crossing into Rust: warm local hits can answer on the
        // next Qt turn, and the signal guard above must already know the key.
        root.wantedCovers = wanted
        if (urls.length > 0) {
            QbzShell.sidebarArtworkWindow(JSON.stringify(urls))
            if (root.artRetriesLeft > 0) {
                root.artRetriesLeft--
                artRetry.restart()
            }
        }
    }

    Timer {
        id: artDispatch
        interval: 70
        repeat: false
        onTriggered: root.requestVisibleCovers()
    }

    Timer {
        id: artRetry
        interval: 350
        repeat: false
        onTriggered: root.requestVisibleCovers()
    }

    // The current cover already has an authoritative resolved path on the
    // player bridge. Reuse it for every row carrying that exact URL while the
    // generic url-keyed artwork echo settles. This matters for adjacent tracks
    // from one album: a missed/late echo otherwise leaves all of them as dark
    // tiles even though the NPB is visibly rendering the same cached file.
    function artUrl(row) {
        var url = row ? (row.artUrl || "") : ""
        return root.kioskHost ? KioskArt.sizedUrl(url, Math.ceil(44 * Screen.devicePixelRatio)) : url
    }
    function coverPath(row) {
        var url = root.artUrl(row)
        var resolved = root.coverMap[url] || ""
        if (resolved !== "")
            return resolved
        if (url !== "" && root.currentRow
                && url === (root.currentRow.artUrl || "")
                && QbzPlayer.npArtworkPath !== "")
            return QbzPlayer.npArtworkPath
        return ""
    }

    // ----------------------------- actions ------------------------------

    function rowBlocked(row) {
        return QbzQConnect.qconnectConnected && row.qconnectCompatible !== true
    }

    function dragAllowed(row) {
        return !root.searchActive && row.phase !== "current" && !root.rowBlocked(row)
    }

    function sectionText(index, row) {
        if (index === 0 && row.phase === "history")
            return QbzSession.tr("HISTORY", QbzSession.trRev)
        if (row.phase === "current")
            return QbzSession.tr("NOW PLAYING", QbzSession.trRev)
        if (row.section === "next-in-queue")
            return QbzSession.tr("NEXT IN QUEUE", QbzSession.trRev)
        if (row.section === "next-up")
            return QbzSession.tr("NEXT UP", QbzSession.trRev)
        return ""
    }

    function queueMenu(row) {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var items = []
        if (row.phase === "upcoming") {
            items.push({ "label": t("Remove from queue", r), "icon": "trash-2",
                         "action": "remove", "external": true })
            items.push({ "label": row.id === (root.doc.stopAfterId || "")
                             ? t("Cancel stop after this", r) : t("Stop after this", r),
                         "icon": "circle-stop", "action": "stop-after", "external": true })
            items.push({ "label": t("Remove all after", r), "icon": "list-x",
                         "action": "remove-after", "external": true })
        } else if (row.phase === "history" && !root.rowBlocked(row)) {
            items.push({ "label": t("Play now", r), "icon": "play-fill", "action": "play" })
        } else if (row.phase === "current") {
            items.push({ "label": row.id === (root.doc.stopAfterId || "")
                             ? t("Cancel stop after this", r) : t("Stop after this", r),
                         "icon": "circle-stop", "action": "stop-after", "external": true })
        }
        if (row.isEphemeral !== true) {
            if (row.isLocal !== true)
                items.push({ "label": t("Add to playlist", r), "icon": "list-plus",
                             "action": "add-to-playlist", "external": true })
            items.push({ "label": t("Track info", r), "icon": "info", "action": "track-info" })
            if (row.isLocal !== true)
                items.push({ "label": row.isFavorite === true
                                 ? t("Remove from Library", r) : t("Add to Library", r),
                             "icon": row.isFavorite === true ? "heart-filled" : "heart",
                             "action": "favorite" })
        }
        if (root.kioskHost && !root.searchActive && row.phase !== "current" && !root.rowBlocked(row)) {
            items.push({label: t("Move up", r), action: "kiosk-up", external: true})
            items.push({label: t("Move down", r), action: "kiosk-down", external: true})
        }
        return items
    }

    function rowPlay(row) {
        if (root.rowBlocked(row))
            return
        if (row.phase === "current")
            QbzPlayer.togglePlay()
        else
            QbzQueue.queueExtendedPlay(row.phase, row.phaseIndex, row.id)
    }

    function menuAction(row, action) {
        if (action === "kiosk-up") { root.moveQueueRow(row, -1); return }
        if (action === "kiosk-down") { root.moveQueueRow(row, 1); return }
        if (action === "remove")
            QbzQueue.queueRemoveUpcomingFlat(row.phaseIndex)
        else if (action === "remove-after")
            QbzQueue.queueRemoveAllAfterFlat(row.phaseIndex)
        else if (action === "stop-after")
            QbzQueue.queueToggleStopAfter(row.id)
        else if (action === "add-to-playlist")
            QbzPlaylistPicker.openForTrack(row.id)
    }

    function historyVisualIndex(row) {
        return (root.doc.historyCount || 0) - 1 - row.phaseIndex
    }

    // The chevrons and pointer drops use insertion slots, not destination row
    // indices. Upcoming stays inside Upcoming. History chevrons reorder the
    // chronological past; a pointer drag may additionally cross Now Playing
    // and move that exact occurrence back into Upcoming.
    function moveQueueRow(row, delta) {
        if (!row || root.searchActive || root.rowBlocked(row))
            return
        root.cancelPlayingFollow()
        var from
        var count
        var targetPhase = row.phase
        if (row.phase === "upcoming") {
            from = row.phaseIndex
            count = root.doc.upcomingCount || 0
        } else if (row.phase === "history") {
            from = root.historyVisualIndex(row)
            count = root.doc.historyCount || 0
        } else {
            return
        }
        if ((delta < 0 && from <= 0) || (delta > 0 && from >= count - 1))
            return
        var slot = delta < 0 ? from - 1 : from + 2
        QbzQueue.queueExtendedDrop(row.phase, row.phaseIndex, row.id,
                                   targetPhase, slot)
    }

    // -------------------------- local reorder ----------------------------

    property string dragPhase: ""
    property int dragPhaseIndex: -1
    property string dragTrackId: ""
    property var dragRow: null
    property int dragRowNumber: 0
    property string dropPhase: ""
    property int dropSlot: -1
    property real dropLineY: -1
    property bool dropHot: false
    readonly property var dragDisplayItem: root.dragRow
        ? Object.assign({}, root.dragRow, { "artPath": root.coverPath(root.dragRow) })
        : ({})

    function beginQueueDrag(row, visualIndex) {
        root.cancelPlayingFollow()
        root.dragPhase = row.phase
        root.dragPhaseIndex = row.phaseIndex
        root.dragTrackId = row.id
        root.dragRow = row
        root.dragRowNumber = visualIndex + 1
        root.dropPhase = ""
        root.dropSlot = -1
        root.dropHot = false
    }

    function clearInlineDrop() {
        root.dropHot = false
        root.dropPhase = ""
        root.dropSlot = -1
        QbzShell.dragInlineVisual = false
    }

    function recomputeDrop() {
        if (!QbzShell.dragActive || root.dragTrackId === "" || root.searchActive) {
            root.clearInlineDrop()
            return
        }
        var contentPoint = queueList.mapFromItem(null, QbzShell.dragX, QbzShell.dragY)
        if (contentPoint.x < 0 || contentPoint.x > queueList.width
                || contentPoint.y < 0 || contentPoint.y > queueList.height) {
            root.clearInlineDrop()
            return
        }
        var cy = queueList.contentY + contentPoint.y
        var index = queueList.indexAt(Math.max(1, queueList.width / 2), cy)
        var insertion
        if (index < 0) {
            insertion = cy <= queueList.originY ? 0 : root.rows.length
        } else {
            var target = queueList.itemAtIndex(index)
            insertion = target && cy >= target.y + target.height / 2 ? index + 1 : index
        }
        var historyCount = root.doc.historyCount || 0
        var hasCurrent = root.doc.currentIndex >= 0
        if (root.dragPhase === "history"
                && (insertion < historyCount
                    || (hasCurrent && insertion === historyCount))) {
            root.dropPhase = "history"
            root.dropSlot = Math.max(0, Math.min(historyCount, insertion))
        } else {
            insertion = Math.max(root.firstUpcoming, insertion)
            root.dropPhase = "upcoming"
            root.dropSlot = Math.max(0, Math.min(root.doc.upcomingCount || 0,
                                                 insertion - root.firstUpcoming))
        }
        root.dropHot = true
        QbzShell.dragInlineVisual = true

        var fullIndex = root.dropPhase === "history"
            ? root.dropSlot : root.firstUpcoming + root.dropSlot
        var lineItem = fullIndex < root.rows.length ? queueList.itemAtIndex(fullIndex) : null
        if (lineItem)
            root.dropLineY = queueList.y + lineItem.y - queueList.contentY
        else {
            var last = queueList.itemAtIndex(root.rows.length - 1)
            root.dropLineY = last ? queueList.y + last.y + last.height
                                      - queueList.contentY
                                      : listHost.height / 2
        }
    }

    function finishQueueDrag() {
        var shouldCommit = root.dropHot && root.dropSlot >= 0
        var phase = root.dragPhase
        var phaseIndex = root.dragPhaseIndex
        var trackId = root.dragTrackId
        var targetPhase = root.dropPhase
        var slot = root.dropSlot
        root.dragPhase = ""
        root.dragPhaseIndex = -1
        root.dragTrackId = ""
        root.dragRow = null
        root.dragRowNumber = 0
        root.dropPhase = ""
        root.dropSlot = -1
        root.dropHot = false
        QbzShell.dragInlineVisual = false
        if (shouldCommit)
            QbzQueue.queueExtendedDrop(phase, phaseIndex, trackId,
                                       targetPhase, slot)
    }

    function dragProxyY() {
        var p = listHost.mapFromItem(null, QbzShell.dragX, QbzShell.dragY)
        return Math.max(queueList.y, Math.min(queueList.y + queueList.height - 50,
                                              p.y - 25))
    }

    Connections {
        target: QbzShell
        function onDragXChanged() { root.recomputeDrop() }
        function onDragYChanged() { root.recomputeDrop() }
        function onDragActiveChanged() {
            if (QbzShell.dragActive)
                root.recomputeDrop()
            else if (root.dragTrackId !== "")
                root.finishQueueDrag()
        }
    }

    Timer {
        interval: 32
        repeat: true
        running: QbzShell.dragActive && root.dragTrackId !== "" && root.dropHot
        onTriggered: {
            var p = listHost.mapFromItem(null, QbzShell.dragX, QbzShell.dragY)
            if (p.y < 34)
                queueList.contentY = Math.max(queueList.originY, queueList.contentY - 18)
            else if (p.y > listHost.height - 34)
                queueList.contentY = Math.min(queueList.originY
                    + Math.max(0, queueList.contentHeight - queueList.height),
                    queueList.contentY + 18)
            root.recomputeDrop()
        }
    }

    CardMenu {
        id: kioskMenu
        kioskHost: root.kioskHost
        menuWidth: Math.min(root.width - 24, 340)
        entries: [
            {label: QbzSession.tr("Clear", QbzSession.trRev), action: "clear", enabled: root.rows.length > 0},
            {label: QbzSession.tr("Add to Playlist", QbzSession.trRev), action: "save", enabled: root.rows.length > 0 && !(root.currentRow && root.currentRow.isEphemeral)},
            {label: QbzSession.tr("Continuous playback", QbzSession.trRev), action: "infinite"},
            {label: QbzSession.tr("30 min", QbzSession.trRev), action: "30"},
            {label: QbzSession.tr("1 hr", QbzSession.trRev), action: "60"},
            {label: QbzSession.tr("2 hr", QbzSession.trRev), action: "120"},
            {label: QbzSession.tr("3 hr", QbzSession.trRev), action: "180"},
            {label: QbzSession.tr("5 hr", QbzSession.trRev), action: "300"},
            {label: QbzSession.tr("Custom…", QbzSession.trRev), action: "custom"},
            {label: QbzSession.tr("Cancel Timer", QbzSession.trRev), action: "cancel", enabled: QbzQueue.sleepActive}
        ]
        onPicked: function(action) {
            if (action === "clear") QbzQueue.queueClear()
            else if (action === "save") QbzQueue.queueSaveAsPlaylist()
            else if (action === "infinite") QbzQueue.queueToggleInfinitePlay()
            else if (action === "cancel") QbzQueue.sleepTimerCancel()
            else if (action === "custom") customSleep.open()
            else QbzQueue.sleepTimerSet(parseInt(action, 10))
        }
    }

    // ------------------------------- UI ---------------------------------

    Column {
        anchors.fill: parent
        spacing: 0

        Rectangle {
            width: parent.width
            height: root.kioskHost ? 64 : 54
            topLeftRadius: theme.radiusMd
            topRightRadius: theme.radiusMd
            color: theme.ambientOn ? theme.surfaceMainA30 : theme.surfaceMain

            Row {
                visible: root.kioskHost
                x: 16
                height: 64
                spacing: 12
                Text { width: 140; height: 64; text: QbzSession.tr("Listen list", QbzSession.trRev); color: theme.textPrimary; font.pixelSize: 20; elide: Text.ElideRight; verticalAlignment: Text.AlignVCenter }
                QbzLineEdit { kioskHost: root.kioskHost; anchors.verticalCenter: parent.verticalCenter; width: Math.max(120, root.width - 256); searchMode: true; text: root.doc.searchQuery || ""; placeholder: QbzSession.tr("Search queue", QbzSession.trRev); onEdited: function(value) { QbzQueue.queueSetSearch(value) } }
                SettingsButton { id: kioskQueueActions; kioskHost: root.kioskHost; btnHeight: 64; minWidth: 64; text: "⋯"; onClicked: kioskMenu.openBelowRight(kioskQueueActions) }
            }

            Row {
                visible: !root.kioskHost
                anchors.left: parent.left
                anchors.leftMargin: theme.spacingMd
                anchors.verticalCenter: parent.verticalCenter
                spacing: theme.spacingSm

                QbzIconButton {
                    btnSize: 32
                    name: "panel-right-close"
                    tooltip: tips
                    tooltipKey: "queue-view-close"
                    tooltipText: QbzSession.tr("Back", QbzSession.trRev)
                    onClicked: QbzShell.navigateBack()
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    visible: root.width >= 720
                    text: QbzSession.tr("Listen list", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightSemibold
                }
                Text {
                    id: queueSummary
                    anchors.verticalCenter: parent.verticalCenter
                    visible: root.width >= 900
                    text: root.rows.length + " " + QbzSession.tr("tracks", QbzSession.trRev)
                        + "  •  " + root.compactDuration(root.queueTiming.total)
                    color: theme.textMuted
                    font.pixelSize: 11
                    MouseArea {
                        id: queueSummaryHover
                        anchors.fill: parent
                        hoverEnabled: true
                        onContainsMouseChanged: tips.hover(containsMouse, queueSummary,
                            "queue-timing", root.queueTimingTooltip)
                    }
                }
            }

            Row {
                visible: !root.kioskHost
                anchors.right: parent.right
                anchors.rightMargin: theme.spacingMd
                anchors.verticalCenter: parent.verticalCenter
                spacing: theme.spacingXs

                QbzLineEdit {
                    width: Math.max(110, Math.min(220, root.width
                        - (root.width >= 720 ? 490 : 390)))
                    searchMode: true
                    text: root.doc.searchQuery || ""
                    placeholder: QbzSession.tr("Search queue", QbzSession.trRev)
                    onEdited: function (value) { QbzQueue.queueSetSearch(value) }
                }
                QbzIconButton {
                    btnSize: 32
                    name: "trash-list"
                    btnEnabled: root.rows.length > 0
                    tooltip: tips
                    tooltipKey: "queue-view-clear"
                    tooltipText: QbzSession.tr("Clear", QbzSession.trRev)
                    onClicked: QbzQueue.queueClear()
                }
                QbzIconButton {
                    btnSize: 32
                    name: "add-to-list"
                    btnEnabled: root.rows.length > 0
                        && !(root.currentRow && root.currentRow.isEphemeral === true)
                    tooltip: tips
                    tooltipKey: "queue-view-save"
                    tooltipText: QbzSession.tr("Add to Playlist", QbzSession.trRev)
                    onClicked: QbzQueue.queueSaveAsPlaylist()
                }
                Row {
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 6

                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Continuous playback", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 11
                    }
                    QbzToggle {
                        anchors.verticalCenter: parent.verticalCenter
                        checked: root.doc.infinitePlay === true
                        onToggled: QbzQueue.queueToggleInfinitePlay()
                    }
                }
                QbzIconButton {
                    id: sleepButton
                    btnSize: 32
                    name: "clock"
                    active: QbzQueue.sleepActive
                    tooltip: tips
                    tooltipKey: "queue-view-sleep"
                    tooltipText: QbzSession.tr("Set Timer", QbzSession.trRev)
                    onClicked: sleepMenu.openAtCursor(sleepButton, 0, sleepButton.height)

                    CardMenu {
                        id: sleepMenu
                        menuWidth: 196
                        entries: QbzQueue.sleepActive
                            ? [{ "label": QbzSession.tr("Cancel Timer", QbzSession.trRev),
                                 "icon": "x", "action": "cancel" }]
                            : [
                                { "label": QbzSession.tr("30 min", QbzSession.trRev), "icon": "clock", "action": "30" },
                                { "label": QbzSession.tr("1 hr", QbzSession.trRev), "icon": "clock", "action": "60" },
                                { "label": QbzSession.tr("2 hr", QbzSession.trRev), "icon": "clock", "action": "120" },
                                { "label": QbzSession.tr("3 hr", QbzSession.trRev), "icon": "clock", "action": "180" },
                                { "label": QbzSession.tr("5 hr", QbzSession.trRev), "icon": "clock", "action": "300" },
                                { "sep": true },
                                { "label": QbzSession.tr("Custom…", QbzSession.trRev), "icon": "clock", "action": "custom" }
                              ]
                        onPicked: function (action) {
                            if (action === "cancel")
                                QbzQueue.sleepTimerCancel()
                            else if (action === "custom")
                                customSleep.open()
                            else
                                QbzQueue.sleepTimerSet(parseInt(action, 10))
                        }
                    }
                    Popup {
                        id: customSleep
                        parent: root.kioskHost ? root : sleepButton
                        x: root.kioskHost ? Math.max(8, root.width - width - 16) : -184
                        y: root.kioskHost ? 64 : sleepButton.height + 6
                        width: 216
                        padding: 12
                        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
                        background: Rectangle {
                            radius: theme.radiusMd
                            color: theme.surfaceCard
                            border.width: 1
                            border.color: theme.borderSubtle
                        }
                        contentItem: Column {
                            spacing: 8
                            Text {
                                text: QbzSession.tr("Stop playback after:", QbzSession.trRev)
                                color: theme.textMuted
                                font.pixelSize: 11
                            }
                            Row {
                                spacing: 8
                                Rectangle {
                                    width: 116
                                    height: root.kioskHost ? 44 : 32
                                    radius: theme.radiusSm
                                    color: theme.surfaceElevated
                                    border.width: 1
                                    border.color: customMinutes.activeFocus
                                        ? theme.accent : theme.borderSubtle
                                    TextInput {
                                        QbzTextEditMenu { }
                                        id: customMinutes
                                        anchors.fill: parent
                                        anchors.leftMargin: 9
                                        anchors.rightMargin: 9
                                        text: "60"
                                        color: theme.textPrimary
                                        font.pixelSize: 12
                                        verticalAlignment: Text.AlignVCenter
                                        validator: IntValidator { bottom: 1; top: 1440 }
                                        inputMethodHints: Qt.ImhDigitsOnly
                                        selectByMouse: true
                                    }
                                }
                                Rectangle {
                                    width: 64
                                    height: root.kioskHost ? 44 : 32
                                    radius: theme.radiusSm
                                    color: setSleepArea.containsMouse ? theme.accentHover : theme.accent
                                    Text {
                                        anchors.centerIn: parent
                                        text: QbzSession.tr("Set", QbzSession.trRev)
                                        color: theme.accentText
                                        font.pixelSize: 12
                                    }
                                    MouseArea {
                                        id: setSleepArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: {
                                            var minutes = parseInt(customMinutes.text, 10)
                                            if (!isNaN(minutes) && minutes > 0)
                                                QbzQueue.sleepTimerSet(minutes)
                                            customSleep.close()
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }

        Item {
            id: listHost
            width: parent.width
            height: parent.height - (root.kioskHost ? 65 : 55)

            ListView {
                id: queueList
                anchors.left: parent.left
                anchors.right: queueScroll.left
                // Reserve a narrow action rail so the two always-visible
                // floating buttons never cover a row or the scrollbar.
                anchors.rightMargin: root.actionRailWidth
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                anchors.leftMargin: theme.spacingMd
                anchors.topMargin: theme.spacingSm
                anchors.bottomMargin: theme.spacingSm
                clip: true
                spacing: 3
                boundsBehavior: Flickable.StopAtBounds
                reuseItems: true
                model: root.rows
                onContentYChanged: root.scheduleVisibleCovers()
                onMovementStarted: {
                    if (!root.programmaticListPosition)
                        root.cancelPlayingFollow()
                }

                delegate: Item {
                    id: rowHost
                    required property var modelData
                    required property int index

                    readonly property string heading: root.sectionText(index, modelData)
                    readonly property var displayItem: Object.assign({}, modelData, {
                        "artPath": root.coverPath(modelData)
                    })
                    readonly property bool isDragSource: root.dragTrackId !== ""
                        && root.dragPhase === modelData.phase
                        && root.dragPhaseIndex === modelData.phaseIndex

                    width: queueList.width
                    height: (heading !== "" ? 24 : 0) + trackRow.height
                    ListView.onPooled: {
                        trackRow.recycleActive = false
                        trackRow.releaseForReuse()
                    }
                    ListView.onReused: trackRow.recycleActive = true

                    Text {
                        visible: rowHost.heading !== ""
                        x: theme.spacingSm
                        width: parent.width - 2 * theme.spacingSm
                        height: 24
                        text: rowHost.heading
                        color: rowHost.modelData.phase === "current"
                            ? theme.accent : theme.textMuted
                        font.pixelSize: 10
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 0.5
                        verticalAlignment: Text.AlignVCenter
                    }

                    TrackRow {
                        id: trackRow
                        kioskHost: root.kioskHost
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.bottom: parent.bottom
                        item: rowHost.displayItem
                        number: rowHost.index + 1
                        showArtwork: true
                        showAlbum: root.width >= 720
                        showFavorite: false
                        showDownload: false
                        showMenu: true
                        showReorder: !root.kioskHost && !root.searchActive
                            && (rowHost.modelData.phase === "upcoming"
                                || rowHost.modelData.phase === "history")
                            && !root.rowBlocked(rowHost.modelData)
                        canMoveUp: rowHost.modelData.phase === "upcoming"
                            ? rowHost.modelData.phaseIndex > 0
                            : (rowHost.modelData.phase === "history"
                                && root.historyVisualIndex(rowHost.modelData) > 0)
                        canMoveDown: rowHost.modelData.phase === "upcoming"
                            ? rowHost.modelData.phaseIndex
                                < (root.doc.upcomingCount || 0) - 1
                            : (rowHost.modelData.phase === "history"
                                && root.historyVisualIndex(rowHost.modelData)
                                    < (root.doc.historyCount || 0) - 1)
                        zebra: true
                        artistLink: true
                        draggable: root.dragAllowed(rowHost.modelData)
                        reorderDrag: true
                        inlineDragVisual: true
                        playBlocked: root.rowBlocked(rowHost.modelData)
                        activeBackground: true
                        overrideActiveMatch: true
                        activeMatch: rowHost.modelData.phase === "current"
                        leadingMarkerIcon: rowHost.modelData.id === (root.doc.stopAfterId || "")
                            ? "circle-stop" : ""
                        menuEntriesOverride: root.queueMenu(rowHost.modelData)
                        artPending: (rowHost.modelData.artUrl || "") !== ""
                            && root.coverPath(rowHost.modelData) === ""
                        skelPhase: (Math.floor(Math.abs(QbzShell.pulseMs) / 900) % 2) === 1
                        artSettleMs: 2500
                        // Keep the MouseArea alive until release while the
                        // complete visual row is carried by `dragProxy`.
                        opacity: rowHost.isDragSource ? 0 : 1

                        onPlayRequested: root.rowPlay(rowHost.modelData)
                        onMoveUpRequested: root.moveQueueRow(rowHost.modelData, -1)
                        onMoveDownRequested: root.moveQueueRow(rowHost.modelData, 1)
                        onBodyDragStarted: root.beginQueueDrag(rowHost.modelData, rowHost.index)
                        onMenuActionRequested: function (action) {
                            root.menuAction(rowHost.modelData, action)
                        }
                    }
                }
            }

            QbzScrollBar {
                id: queueScroll
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                target: queueList
                onUserScrollStarted: root.cancelPlayingFollow()
            }

            // Always-visible viewport actions. They occupy the reserved rail,
            // while QbzContextMenu reparents to Overlay.overlay and therefore
            // remains above them when opened near the lower-right corner.
            Column {
                id: viewportActions
                z: 15
                anchors.right: queueScroll.left
                anchors.rightMargin: 8
                anchors.bottom: parent.bottom
                anchors.bottomMargin: theme.spacingMd
                spacing: 8

                Rectangle {
                    width: root.kioskHost ? 64 : 36
                    height: root.kioskHost ? 64 : 36
                    radius: theme.radiusSm
                    color: theme.surfaceCard
                    border.width: 1
                    border.color: root.followPlaying ? theme.accent
                                                     : theme.borderSubtle
                    QbzIconButton {
                        anchors.centerIn: parent
                        btnSize: root.kioskHost ? 64 : 34
                        iconSize: 16
                        name: "disc-3"
                        active: root.followPlaying
                        btnEnabled: root.hasPlayingRow
                        tooltip: tips
                        tooltipKey: "queue-focus-playing"
                        tooltipText: QbzSession.tr("Focus on playing",
                                                   QbzSession.trRev)
                        onClicked: root.focusOnPlaying()
                    }
                }

                Rectangle {
                    width: root.kioskHost ? 64 : 36
                    height: root.kioskHost ? 64 : 36
                    radius: theme.radiusSm
                    color: theme.surfaceCard
                    border.width: 1
                    border.color: theme.borderSubtle
                    QbzIconButton {
                        anchors.centerIn: parent
                        btnSize: root.kioskHost ? 64 : 34
                        iconSize: 16
                        name: "chevron-up"
                        btnEnabled: root.rows.length > 0
                        tooltip: tips
                        tooltipKey: "queue-go-top"
                        tooltipText: QbzSession.tr("Go to top", QbzSession.trRev)
                        onClicked: root.goToTop()
                    }
                }
            }

            // QueueView's drag visual is the row itself, not the generic
            // title/subtitle pill. The source delegate stays alive but fully
            // transparent so it retains the mouse grab; this identical row
            // follows the pointer over the list. Leaving the list hides it and
            // re-enables AppShell's compact ghost for sidebar playlist drops.
            TrackRow {
                id: dragProxy
                visible: QbzShell.dragActive && root.dropHot && root.dragRow !== null
                enabled: false
                z: 20
                x: queueList.x
                y: root.dragProxyY()
                width: queueList.width
                item: root.dragDisplayItem
                number: root.dragRowNumber
                showArtwork: true
                showAlbum: root.width >= 720
                showFavorite: false
                showDownload: false
                showMenu: true
                showReorder: root.dragRow !== null
                    && (root.dragRow.phase === "upcoming"
                        || root.dragRow.phase === "history")
                    && !root.searchActive
                    && !root.rowBlocked(root.dragRow)
                canMoveUp: root.dragRow !== null
                    && (root.dragRow.phase === "upcoming"
                        ? root.dragRow.phaseIndex > 0
                        : (root.dragRow.phase === "history"
                            && root.historyVisualIndex(root.dragRow) > 0))
                canMoveDown: root.dragRow !== null
                    && (root.dragRow.phase === "upcoming"
                        ? root.dragRow.phaseIndex < (root.doc.upcomingCount || 0) - 1
                        : (root.dragRow.phase === "history"
                            && root.historyVisualIndex(root.dragRow)
                                < (root.doc.historyCount || 0) - 1))
                zebra: true
                artistLink: true
                draggable: false
                reorderDrag: false
                playBlocked: root.dragRow !== null && root.rowBlocked(root.dragRow)
                activeBackground: true
                overrideActiveMatch: true
                activeMatch: root.dragRow !== null && root.dragRow.phase === "current"
                leadingMarkerIcon: root.dragRow !== null
                    && root.dragRow.id === (root.doc.stopAfterId || "")
                    ? "circle-stop" : ""
                menuEntriesOverride: root.dragRow !== null
                    ? root.queueMenu(root.dragRow) : []
                artPending: root.dragRow !== null
                    && (root.dragRow.artUrl || "") !== ""
                    && root.coverPath(root.dragRow) === ""
                skelPhase: (Math.floor(Math.abs(QbzShell.pulseMs) / 900) % 2) === 1
                artSettleMs: 2500
            }

            Rectangle {
                visible: root.dropHot && root.dropLineY >= 0
                z: 30
                x: queueList.x + theme.spacingSm
                y: Math.max(0, Math.min(parent.height - height, root.dropLineY))
                width: queueList.width - 2 * theme.spacingSm
                height: 2
                radius: 1
                color: theme.accent
            }

            QbzEmptyState {
                visible: root.rows.length === 0
                anchors.centerIn: parent
                iconName: root.searchActive ? "search" : "list-music"
                title: root.searchActive
                    ? QbzSession.tr("No tracks match your search", QbzSession.trRev)
                    : QbzSession.tr("Your queue is empty", QbzSession.trRev)
                body: root.searchActive ? ""
                    : QbzSession.tr("Play an album or track to get started", QbzSession.trRev)
            }
        }
    }

    QbzTooltip {
        id: tips
        anchors.fill: parent
        z: 4000
    }
}
