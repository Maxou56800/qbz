// Discography Builder — the full-page flow that assembles an artist's complete
// discography into a saved `kind='artist_collection'` collection. QML port of
// myqbz/DiscographyBuilderView.slint:418-787 (spec 01 §8).
//
// Route "discobuilder". Entry: the artist page's overflow -> "Create Artist
// Collection". Exit: the footer Cancel — there is NO per-view back button
// (:438: the shell owns the global < > nav).
//
// TWO-PART SHELL (:8-11, :422-428, :727-736): everything scrolls except a
// FIXED 60px footer pinned at the bottom as a sibling of the scroller (NOT
// sticky-inside-scroll), so the last rows never peek past it.
//
// WINDOWING (spec 01 §8.9): a discography is tens-to-hundreds of groups and
// every row mounts a QualityBadgeFull, so the table is ONE recycled ListView
// over the FLATTENED row descriptors Rust publishes as `doc.rows`
// (`{kind, groupKey, groupIsCompilation, restOpacity, candidate}`), with
// `cacheBuffer: 44*10`. NOT a ListView of groups each holding a Repeater of
// alternates: that shape leaves `rows` unread and makes every delegate a
// variable-height Column. Everything above the table — the artist header,
// the NAME field, the ORDER BY row, the 20px spacer, the loading/error/empty
// band and the table's own rule + column header — is the ListView.header, so
// ONE scroller drives the page; the content's 24px bottom padding is the
// ListView.footer. In the three non-populated states the model is empty and
// the header carries the whole page: the ListView is never branched out of the
// tree, because a new `model` assignment resets contentY (recipe §8).
//
// The root is `color: "transparent"` VERBATIM (:419) — not the ambient ternary
// the other views use, and therefore no `radius: 12` either: there is no fill
// of its own to round, the content frame's surfaceMain shows through, and the
// footer's surfaceMain matches it (AppShell's bezel nubs own the corners).

import QtQuick
import "../../kiosk"
import com.blitzfc.qbz
import "../../controls"
import "../../theme"
import "../local"

Rectangle {
    id: root
    property bool kioskHost: false
    color: "transparent"

    QbzTheme { id: theme }

    property var doc: ({})
    function parseDoc() {
        try {
            return JSON.parse(QbzDisco.discoJson)
        } catch (e) {
            return ({})
        }
    }

    // `rows` is a plain JS array, so every Rust publish assigns a brand-new
    // ListView model. Capture the viewport BEFORE assigning `doc`; trying to
    // recover it from onRowsChanged is already too late because Qt may have
    // reset contentY during the binding update.
    property int documentEpoch: 0
    function applyDocument() {
        var next = root.parseDoc()
        var currentArtist = root.doc.artistId || ""
        var nextArtist = next.artistId || ""
        var preserve = nextArtist !== "" && nextArtist === currentArtist
            && next.loading !== true
        var savedY = preserve ? rowList.contentY : 0
        root.documentEpoch += 1
        var epoch = root.documentEpoch
        root.doc = next
        Qt.callLater(function () {
            if (epoch !== root.documentEpoch)
                return
            rowList.forceLayout()
            var minY = rowList.originY
            var maxY = minY + Math.max(0,
                rowList.contentHeight - rowList.height)
            rowList.contentY = preserve
                ? Math.max(minY, Math.min(savedY, maxY)) : minY
        })
    }
    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // --- Document slices (spec 02 §5.8 DiscoDoc) --------------------------
    readonly property string artistId: doc.artistId || ""
    readonly property bool loading: doc.loading === true
    readonly property string loadError: doc.loadError || ""
    readonly property var groups: doc.groups || []
    // The flattened table (spec 01 §8.9) — one entry per RENDERED row, in the
    // same order as `groups`, each carrying its precomputed `restOpacity`.
    readonly property var rows: doc.rows || []
    readonly property string orderBy: doc.orderBy || "release_date"
    readonly property int selectedCount: doc.selectedCount || 0
    readonly property int groupCount: doc.groupCount || 0
    readonly property bool creating: doc.creating === true
    readonly property string defaultName: doc.defaultName || ""
    // Precedence exactly as the Slint writes it (:599,610,621-623,634-636):
    // loading -> loadError -> empty -> the table.
    readonly property bool populated: !root.loading && root.loadError === ""
        && root.groups.length > 0

    // --- Collection name (QML-local) --------------------------------------
    // The bridge (spec 02 §4.3) has no `setName`: `create(name)` takes it, so
    // the field's truth lives here and Rust seeds it once through
    // `defaultName` ("{artist} — Complete Discography").
    // DIVERGENCE from spec 01 §8.7, forced by that surface: Slint cannot trim
    // and exposes `name-trimmed-empty` from Rust; with one implementation
    // there is nothing to diverge from, so the trim is done here.
    property string collectionName: ""
    property bool nameEdited: false
    readonly property bool nameTrimmedEmpty: root.collectionName.trim() === ""
    // A new artist re-seeds; a late-arriving default (the name resolves after
    // the fetch) must not clobber what the user typed.
    onArtistIdChanged: {
        root.nameEdited = false
        root.collectionName = root.defaultName
    }
    onDefaultNameChanged: if (!root.nameEdited) root.collectionName = root.defaultName
    // Slint's `reset` blanks `collection-name` before EVERY fetch
    // (myqbz_builder.rs:690) and `install` re-seeds it, so a name the user typed
    // during a previous open never survives into the next one. Without this the
    // `nameEdited` latch keeps a stale edit forever when the builder is
    // re-opened for the SAME artist (`artistId` and `defaultName` both unchanged,
    // so neither handler above fires).
    onLoadingChanged: if (root.loading) {
        root.nameEdited = false
        root.collectionName = ""
    }
    Component.onCompleted: {
        root.applyDocument()
        if (!root.nameEdited && root.collectionName === "")
            root.collectionName = root.defaultName
    }

    // Footer count. Rust pre-renders the ONE true plural in this domain with
    // `qbz_i18n::tf` and publishes it as `footerCount` (spec 01 §4/§8.7) —
    // QML has no plural entry point. The fallback below only runs if that key
    // is absent, and then falls through to the English source (an untranslated
    // msgid renders, it does not break — recipe §7).
    readonly property string footerText: {
        var pre = root.doc.footerCount || ""
        if (pre !== "")
            return pre
        var src = root.groupCount === 1
            ? "{} of {} album selected" : "{} of {} albums selected"
        return root.t(src).replace("{}", String(root.selectedCount))
                          .replace("{}", String(root.groupCount))
    }

    /// The six column labels (:677-700) — uppercase literals for the same
    /// reason as the eyebrow, `letterSpacing 1.2`, and the per-cell alignment
    /// the Slint gives each one. `titleW` is the 1fr ALBUM cell.
    function headCells(rev, titleW) {
        return [
            { "w": 56, "label": QbzSession.tr("YEAR", rev), "center": false },
            { "w": 72, "label": QbzSession.tr("TYPE", rev), "center": false },
            { "w": titleW, "label": QbzSession.tr("ALBUM", rev), "center": false },
            { "w": 72, "label": QbzSession.tr("TRACKS", rev), "center": true },
            { "w": 60, "label": QbzSession.tr("SOURCE", rev), "center": true },
            { "w": 156, "label": QbzSession.tr("QUALITY", rev), "center": true }
        ]
    }

    /// ORDER BY segments (:552-555). Built by a function that TAKES `rev` so
    /// the array rebuilds on a live language switch.
    function orderSegments(rev) {
        return [
            { "id": "release_date", "label": QbzSession.tr("Release date", rev) },
            { "id": "title", "label": QbzSession.tr("Title", rev) },
            { "id": "manual", "label": QbzSession.tr("Manual", rev) }
        ]
    }

    // --- Everything above the table (the ListView header) ------------------
    Component {
        id: pageHead

        Column {
            width: rowList.width
            spacing: 0

            // content padding-top (:433).
            Item { width: 1; height: 24 }

            // ── header.builder-header (:442-486) ──
            Row {
                width: parent.width
                spacing: 20
                // 72px circle. The Slint draws NO placeholder glyph — an
                // artist without a portrait is the bare surface (:446-458).
                Rectangle {
                    width: 72
                    height: 72
                    radius: 36
                    color: theme.surfaceElevated
                    clip: true
                    Loader { anchors.fill: parent; sourceComponent: root.kioskHost ? kioskImage9049 : desktopImage9049
 Component { id: kioskImage9049; KioskArtwork {
                        anchors.fill: parent
                        visible: (root.doc.artistAvatarUrl || "") !== ""
                        source: root.doc.artistAvatarPath || ""
                        radius: 36
                    } }
 Component { id: desktopImage9049; RoundedImage {
                        anchors.fill: parent
                        visible: (root.doc.artistAvatarUrl || "") !== ""
                        source: root.doc.artistAvatarPath || ""
                        radius: 36
                    } }
 }
                }
                Column {
                    width: Math.max(0, parent.width - 72 - 20)
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 4
                    // Uppercase in the msgid: Slint has no text-transform and
                    // writes the literal in caps (:465-471), and the eight
                    // catalogues carry it that way.
                    Text {
                        text: root.t("BUILD ARTIST COLLECTION")
                        color: theme.textMuted
                        font.pixelSize: 10
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 1.5
                    }
                    Text {
                        width: parent.width
                        text: (root.doc.artistName || "") === ""
                            ? "—" : root.doc.artistName
                        color: theme.textPrimary
                        font.pixelSize: 28
                        font.weight: theme.weightBold
                        elide: Text.ElideRight
                    }
                }
            }
            Item { width: 1; height: 24 }

            // ── label.field — the collection-name input (:489-524) ──
            Column {
                width: parent.width
                spacing: 8
                Text {
                    text: root.t("NAME")
                    color: theme.textMuted
                    font.pixelSize: 10
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 1.5
                }
                // Inlined rather than QbzLineEdit's plain arm: that arm is
                // 240x34 / borderSubtle and this field is a one-off 540x40
                // bordered `surfaceElevated` (spec 01 §8.3). Do not fork the
                // shared control for it.
                Rectangle {
                    width: 540
                    height: root.kioskHost ? 44 : 40
                    color: theme.surfaceCard
                    radius: 8
                    border.width: 1
                    border.color: nameField.activeFocus
                        ? theme.accent : theme.surfaceElevated
                    TextInput {
                        QbzTextEditMenu { }
                        id: nameField
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        color: theme.textPrimary
                        font.pixelSize: 14
                        verticalAlignment: Text.AlignVCenter
                        clip: true
                        selectByMouse: true
                        text: root.collectionName
                        onTextEdited: {
                            root.nameEdited = true
                            root.collectionName = text
                        }
                        // Re-seed from Rust while the field is NOT being
                        // edited (the QbzLineEdit.qml:174-179 idiom).
                        Binding {
                            target: nameField
                            property: "text"
                            value: root.collectionName
                            when: !nameField.activeFocus
                        }
                    }
                }
            }
            Item { width: 1; height: 16 }

            // ── .order-row — the segmented control (:527-594) ──
            Row {
                width: parent.width
                spacing: 12
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.t("ORDER BY")
                    color: theme.textMuted
                    font.pixelSize: 10
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 1.5
                }
                // Use the shared labelled segmented control. The former
                // one-off Rectangle did not set a fill, so Qt's default white
                // leaked through instead of the theme well.
                QbzTabBar {
                    kioskHost: root.kioskHost
                    anchors.verticalCenter: parent.verticalCenter
                    tabs: root.orderSegments(QbzSession.trRev)
                    activeId: root.orderBy
                    counts: false
                    underline: false
                    compact: false
                    onSelected: function (id) { QbzDisco.setOrder(id) }
                }
            }
            Item { width: 1; height: 20 }

            // ── loading / error / empty — one 96px band (:597-632) ──
            Item {
                width: parent.width
                visible: !root.populated
                height: visible ? 96 : 0
                Text {
                    anchors.fill: parent
                    text: root.loading
                        ? root.t("Loading...")
                        : (root.loadError !== ""
                            ? root.loadError : root.t("No results found"))
                    // The Slint uses Theme.favorite here, not Theme.danger
                    // (:614) — keep it.
                    color: (!root.loading && root.loadError !== "")
                        ? theme.favorite : theme.textMuted
                    font.pixelSize: 14
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
            }

            // ── .groups table preamble: border-top + 4px pad (:639-640) ──
            Rectangle {
                width: parent.width
                visible: root.populated
                height: visible ? 1 : 0
                color: theme.surfaceElevated
            }
            Item { width: 1; visible: root.populated; height: visible ? 4 : 0 }

            // ── header row (.row.header-row, :643-702) ──
            // The 7-column template is inlined here and in DiscoCandidateRow —
            // a MATCHED PAIR. 28/56/72/1fr/72/60/156, padding 8, spacing 10.
            Item {
                id: colHead
                width: parent.width
                visible: root.populated
                height: visible ? (root.kioskHost ? 44 : 36) : 0
                readonly property real titleW: Math.max(
                    0, colHead.width - 16 - 444 - 60)

                Rectangle {
                    y: parent.height - 1
                    width: parent.width
                    height: 1
                    color: theme.surfaceElevated
                }
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    anchors.rightMargin: 8
                    spacing: 10

                    // [0] 28px — tri-state select-all. The indeterminate state
                    // renders minus.svg inside the same accent disc (the
                    // SelectCheck/TreeRow idiom), never a hand-drawn dash.
                    Item {
                        width: root.kioskHost ? 60 : 28
                        height: parent.height
                        SelectCheck {
                            diameter: root.kioskHost ? 44 : 13
                            anchors.left: parent.left
                            anchors.verticalCenter: parent.verticalCenter
                            on: root.doc.allChecked === true
                            partial: root.doc.allChecked !== true
                                && root.doc.someChecked === true
                            onToggled: QbzDisco.toggleAll()
                        }
                    }
                    // [1..6] — the column labels, in order.
                    Repeater {
                        model: root.headCells(QbzSession.trRev, colHead.titleW)
                        delegate: Text {
                            required property var modelData
                            width: modelData.w
                            height: parent.height
                            text: modelData.label
                            color: theme.textMuted
                            font.pixelSize: 10
                            font.weight: theme.weightSemibold
                            font.letterSpacing: 1.2
                            horizontalAlignment: modelData.center
                                ? Text.AlignHCenter : Text.AlignLeft
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                    }
                }
            }
        }
    }

    // content padding-bottom (:436).
    Component {
        id: bottomPad
        Item { width: 1; height: 24 }
    }

    // --- The scroller (:424-428) -----------------------------------------
    ListView {
        id: rowList
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.bottom: footerBar.top
        // content padding-l/r 32 (:431-432); the scrollbar gutter sits inside
        // the right padding, so it never overlaps a row.
        anchors.leftMargin: 32
        anchors.rightMargin: 32
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        // spec 01 §8.9: 44 * 10.
        cacheBuffer: root.kioskHost ? 128 : 440
        model: root.populated ? root.rows : []
        header: pageHead
        footer: bottomPad

        // ONE row — a group's primary, or one of its alternates (:705-722).
        // Every field comes off the Rust row descriptor: `restOpacity` is
        // precomputed there (primary = isCompilation ? 0.65 : 1.0, alternate =
        // 0.72 unconditionally), so it is never re-derived here.
        delegate: DiscoCandidateRow {
            kioskHost: root.kioskHost
            required property var modelData
            width: rowList.width
            candidate: modelData.candidate || ({})
            groupKey: modelData.groupKey || ""
            alt: modelData.kind === "alt"
            groupIsCompilation: modelData.groupIsCompilation === true
            restOpacity: modelData.restOpacity !== undefined
                ? modelData.restOpacity : 1.0
        }
    }

    Connections {
        target: QbzDisco
        function onDiscoJsonChanged() { root.applyDocument() }
    }

    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: rowList; scope: "discobuilder" }
    QbzScrollBar {
        target: rowList
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: rowList.top
        anchors.bottom: rowList.bottom
    }

    // --- footer.builder-footer (:728-785) --------------------------------
    Rectangle {
        id: footerBar
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: root.kioskHost ? 72 : 60
        color: theme.surfaceMain

        Rectangle {
            y: 0
            width: parent.width
            height: 1
            color: theme.surfaceElevated
        }
        Text {
            anchors.left: parent.left
            anchors.leftMargin: 32
            anchors.verticalCenter: parent.verticalCenter
            width: Math.max(0, parent.width - 32 - 88 - actionRow.width - 16)
            text: root.footerText
            color: theme.textSecondary
            font.pixelSize: 13
            elide: Text.ElideRight
        }
        Row {
            id: actionRow
            // Asymmetric right padding (:739) — 88 clears the back-to-top FAB.
            anchors.right: parent.right
            anchors.rightMargin: root.kioskHost ? 16 : 88
            anchors.verticalCenter: parent.verticalCenter
            spacing: 8
            SettingsButton {
                text: root.t("Cancel")
                btnHeight: root.kioskHost ? 64 : 36
                minWidth: 0
                onClicked: QbzDisco.back()
            }
            QbzPrimaryButton {
                label: root.creating
                    ? root.t("Loading...") : root.t("Create Collection")
                btnHeight: root.kioskHost ? 64 : 36
                btnEnabled: !(root.creating || root.selectedCount === 0
                    || root.nameTrimmedEmpty)
                onClicked: QbzDisco.create(root.collectionName)
            }
        }
    }
}
