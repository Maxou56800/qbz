// Artist discography page — QML port of artist/ArtistReleasesView.slint (257
// lines, read in full for this port): the full, paginated grid for ONE release
// bucket of one artist, reached from the artist page's "See discography" link
// and from the album page's "From the same artist" View all.
//
// (Before this file existed, "See discography" was a fully lit, pointer-cursor
// Text with an EMPTY MouseArea under a POC-NOTE saying the page was out of
// scope. That note and that emptiness are both gone.)
//
// Layout, read off the .slint:
//   :36-103  fixed 56px header — artist kicker + bucket title LEFT, the sort
//            select RIGHT, both vertically centred on y=25
//   :131-138 body Column padding 32 / 32, top 8, bottom 100, spacing 0
//   :121-129 originally used infinite scroll. Qt uses the shared explicit
//            Load-more control so a tail publish never changes the Flickable
//            extent from inside its momentum handler.
//   :140-203 THREE mutually exclusive body states — loading, error+Retry,
//            empty — then the grid, then the tail spinner
//   :205-231 200x266 cards, gap 24, view-mode hard-wired "grid"
//   :242-256 scrollbar below the header
//
// WHAT THIS PAGE DELIBERATELY DOES NOT HAVE (checked against the .slint, not
// against its cousins): no search field, no group-by, no Hi-Res filter and no
// grid/list toggle. The explicit Load-more affordance is a Qt performance
// policy shared by every paginated View-all; one sort select remains the
// entire toolbar.
//
// NAV BUTTONS: the .slint draws a NavButtons pair at x=32 and starts its text
// stack 16px after it. In this port back/forward live in the shell header
// (HeaderBar.qml:251-266), so the stack starts at x=48 — the 32 + 0 + 16 that
// DiscoverBrowseView.qml:14-18 already establishes as the convention. The back
// affordance is present, it is just drawn once for the whole app.
//
// OFFLINE: shell/AppShell.slint:503-511 mounts the whole view under
// `!OfflineState.offline`, so it simply does not exist offline. This port has
// no per-route unmount, so the page honours the same gate with the seam it does
// have — QbzOfflinePlaceholder over a hidden body — and `artist_releases_qt::
// open` refuses to navigate here offline in the first place.
//
// Known navigation constraint:
//  - back/forward re-mounts this view against whatever document is still on
//    QbzArtist.artistReleasesJson rather than re-fetching, because the port's
//    nav stack stores no payload (nav_qt.rs:9-23). That is exactly how
//    `labelreleases` already behaves; the Rust generation guard is what keeps a
//    late page from painting into the wrong bucket.
// Scroll position itself is restored by the shared ScrollMemory below.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    property bool kioskHost: false

    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    radius: 12

    QbzTheme { id: theme }

    // ONE document, re-parsed on every publish — a fresh string is a fresh
    // object, so the Repeaters below always see a NEW reference ("Rebind
    // requires a NEW object reference", cards/PlaylistCollage.qml). Nothing in
    // this file ever mutates `doc` in place.
    readonly property var doc: {
        try {
            return JSON.parse(QbzArtist.artistReleasesJson)
        } catch (e) {
            return {}
        }
    }
    readonly property var albums: doc.albums || []

    // .slint:85-99 — the five options in the reference's order, and the index
    // <-> wire-key round trip. "A–Z"/"Z–A" carry a U+2013 EN DASH: the msgid in
    // the catalogue is the en-dash form, and a hyphen would go untranslated in
    // all seven non-English locales. (LibraryToolbar's "Title A-Z" is a
    // DIFFERENT, hyphenated msgid — do not copy it here.)
    readonly property var sortLabels: [
        QbzSession.tr("Default", QbzSession.trRev),
        QbzSession.tr("Newest", QbzSession.trRev),
        QbzSession.tr("Oldest", QbzSession.trRev),
        QbzSession.tr("A–Z", QbzSession.trRev),
        QbzSession.tr("Z–A", QbzSession.trRev),
        QbzSession.tr("Popularity", QbzSession.trRev),
    ]
    // These five strings are BOTH what gets persisted (artist_prefs.rs, the
    // store the artist page's per-section picker shares) and what the sort
    // functions switch on, on either side of the bridge.
    readonly property var sortKeys: ["default", "newest", "oldest", "title-asc", "title-desc", "relevant"]
    // .slint:24-28. A BINDING, never an assignment: QbzSelect does not
    // self-assign currentIndex (QbzSelect.qml:299-300), and this is what seats
    // the picker on the sort the page opened with — which is the user's
    // persisted choice for this bucket, not "Default".
    readonly property int sortIndex: Math.max(0, root.sortKeys.indexOf(root.doc.sortBy || "default"))

    // ============================ offline gate ============================
    QbzOfflinePlaceholder {
        visible: QbzSession.offline
        anchors.centerIn: parent
        showSettingsAction: true
        onSettingsClicked: QbzShell.navigateTo("settings")
    }

    Column {
        anchors.fill: parent
        spacing: 0
        visible: !QbzSession.offline

        // --- Fixed 56px header -------------------------------------------
        Item {
            id: header
            width: parent.width
            height: root.kioskHost ? 64 : 56

            Rectangle {
                anchors.fill: parent
                // Rounded at the TOP because this bar is full-bleed at y=0 of
                // the content pane, and under the dynamic background the pane's
                // own rounding cannot reach it: Qt's `clip` is a rectangular
                // scissor that ignores `radius`, and AppShell hides its bezel
                // nubs while the field is meant to show through the corners.
                // So a full-bleed pane child rounds ITSELF — AppShell.qml says
                // exactly this ("there is no mask that can do it for it here")
                // and this bar was the counterexample the owner spotted in
                // Discover. Invisible with the background off: the view root
                // paints the same colour underneath.
                topLeftRadius: theme.radiusMd
                topRightRadius: theme.radiusMd
                color: root.ambientOn ? theme.surfaceMainA30 : theme.surfaceMain
            }

            // .slint:47-70 — a COLUMN, spacing 2. (A Row would top-align its
            // children; this stack is vertical precisely so the kicker sits on
            // the title.) `y: 25 - height/2` is the header's centring
            // convention — 25, not 28, and it is the .slint's own value at :43.
            Column {
                x: 48
                y: (root.kioskHost ? 32 : 25) - height / 2
                spacing: 2

                // The artist name. SERVER TEXT — never translated (.slint:54
                // says so in as many words).
                Text {
                    text: root.doc.name || ""
                    color: theme.textMuted
                    font.pixelSize: 11
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 0.6
                    elide: Text.ElideRight
                    width: Math.max(0, sortSel.x - 48 - 16)
                }
                // The bucket title, translated in RUST
                // (artist_qt::release_type_title) because it travels inside the
                // JSON document — hence artist_releases_qt::republish on a
                // language change.
                //
                // The width cap is not decoration: "Compilations" in German
                // against a narrow window would otherwise run under the sort
                // select, which draws after it and therefore on top of it.
                Text {
                    text: root.doc.title || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightBold
                    elide: Text.ElideRight
                    width: Math.max(0, sortSel.x - 48 - 16)
                }
            }

            // .slint:74-102. `sm: true` + `menuWidth: 150` are the reference's
            // own values verbatim: QbzSelect.qml:34 is `width = menuWidth - 40`
            // under `sm`, so 150 lands at the 110 x 30 the reference computes
            // (QbzSelect.slint:88). Passing 190 "to get 150" is the exact
            // mistake that file's :42-50 note documents.
            QbzSelect {
                        kioskHost: root.kioskHost
                id: sortSel
                x: parent.width - width - 32
                y: (root.kioskHost ? 32 : 25) - height / 2
                sm: true
                menuWidth: 150
                options: root.sortLabels
                currentIndex: root.sortIndex
                onSelected: function (i) {
                    QbzArtist.releasesSetSort(root.sortKeys[i] || "default")
                }
            }
        }

        // --- Scrolling grid ------------------------------------------------
        Item {
            width: parent.width
            height: parent.height - (root.kioskHost ? 64 : 56)

            Flickable {
                id: flick
                anchors.fill: parent
                // A rectangular scissor — it ignores `radius`. The root's own
                // radius-12 fill is what rounds this pane to the AppShell bezel.
                clip: true
                contentWidth: width
                contentHeight: page.implicitHeight
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: page
                    width: parent.width
                    leftPadding: 32
                    rightPadding: 32
                    topPadding: 8
                    bottomPadding: 100
                    spacing: 0

                    // --- State 1: first page in flight (.slint:140-156) ----
                    Column {
                        visible: root.doc.loading === true
                        anchors.horizontalCenter: parent.horizontalCenter
                        spacing: 12

                        QbzSpinner {
                            size: 36
                            anchors.horizontalCenter: parent.horizontalCenter
                        }
                        Text {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: QbzSession.tr("Loading…", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 14
                            horizontalAlignment: Text.AlignHCenter
                        }
                    }

                    // --- State 2: the first page failed (.slint:158-193) ---
                    // Only the FIRST page raises this; a failed tail page keeps
                    // what is on screen and just stops the tail spinner
                    // (main.rs:15129 vs :3599).
                    Column {
                        visible: root.doc.loading !== true && root.doc.loadError === true
                        anchors.horizontalCenter: parent.horizontalCenter
                        spacing: 12

                        Text {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: QbzSession.tr("Failed to load Library", QbzSession.trRev)
                            color: theme.textSecondary
                            font.pixelSize: 14
                            horizontalAlignment: Text.AlignHCenter
                        }
                        // .slint:169-191 — 34 tall, radius 6, 16px horizontal
                        // padding, elevated -> hover fill.
                        Rectangle {
                            anchors.horizontalCenter: parent.horizontalCenter
                            width: retryLabel.implicitWidth + 32
                            height: 34
                            radius: 6
                            color: retryArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated

                            Text {
                                id: retryLabel
                                anchors.centerIn: parent
                                text: QbzSession.tr("Retry", QbzSession.trRev)
                                color: theme.textPrimary
                                font.pixelSize: 13
                                font.weight: theme.weightMedium
                            }
                            MouseArea {
                                id: retryArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: QbzArtist.releasesRetry()
                            }
                        }
                    }

                    // --- State 3: loaded, and the bucket is empty ----------
                    // (.slint:195-203. A sibling of the grid, not an arm of it.)
                    Text {
                        visible: root.doc.loading !== true
                            && root.doc.loadError !== true
                            && root.albums.length === 0
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: QbzSession.tr("No releases found.", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 14
                    }

                    // --- The grid (.slint:205-231) -------------------------
                    // `viewMode: "grid"` is HARD-WIRED, as in the reference
                    // (:207): this page has no toggle.
                    AlbumCollection {
                kioskHost: root.kioskHost
                        id: collection
                        visible: root.doc.loading !== true && root.doc.loadError !== true
                        width: parent.width - 64
                        // Identity of the catalog on screen. Opening ANOTHER
                        // bucket (or another artist) disarms a tail fade that
                        // was armed by a scroll whose page never landed, so the
                        // new grid's first paint is never coloured by it.
                        collectionKey: (root.doc.id || "") + ":" + (root.doc.releaseType || "")
                        albums: root.albums
                        viewMode: "grid"
                        cardWidth: 200
                        cardHeight: 266
                        cardGap: 24
                        flick: flick
                        // The page Column's padding-top. The loading / error
                        // branches are mutually exclusive with the grid, so
                        // nothing else sits above it (.slint:213-222).
                        contentOffset: root.kioskHost ? y : (8)
                    }

                    Item {
                        visible: root.albums.length > 0 && root.doc.hasMore === true
                        width: parent.width - 64
                        height: visible ? loadMore.height : 0

                        QbzLoadMore {
                            id: loadMore
                            width: parent.width
                            buttonHeight: root.kioskHost ? 64 : 32
                            busy: root.doc.loadMoreLoading === true
                            skeleton: "cards"
                            cellW: 224
                            cellH: 290
                            onClicked: {
                                // Arm before the bridge call: a cached page
                                // may republish synchronously.
                                collection.armTailFade()
                                QbzArtist.releasesLoadMore()
                            }
                        }
                    }
                }
            }

            // Tracks only the scrolling grid — the header is a sibling above,
            // which is the .slint's `y: 56px` / `height: parent.height - (root.kioskHost ? 64 : 56)px`.
            // Back/forward scroll memory (controls/ScrollMemory.qml): reports
            // this container's offset while it is the live page, and restores it
            // when a back/forward step arms this route.
            ScrollMemory { target: flick; scope: "artistreleases" }
            QbzScrollBar {
                anchors.right: parent.right
                anchors.rightMargin: 4
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                target: flick
            }
        }
    }
}
