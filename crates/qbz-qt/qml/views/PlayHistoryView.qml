// Recently Played Albums + Most Played Albums — ONE view for the two
// local-history "View all" pages, because discover/RecentAlbumsView.slint
// and discover/MostPlayedAlbumsView.slint are the same page with five
// differences (the .slint keeps them apart only because Slint has no
// runtime-armed component). `mode` on the published document arms them:
//
//                    | recent                     | mostPlayed
//   title            | "Recently Played Albums"   | "Most Played Albums"
//   card height      | 266                        | 286  (the "{} plays" line)
//   search box       | none                       | 240x34, right-anchored
//   root background  | ambient-aware (:31)        | surface-main (:26)
//   header backgrnd  | surface-main (:43)         | surface-main (:34)
//
// Everything else is byte-identical, INCLUDING the loading block (spinner 36
// + @tr("Loading…")) and the empty msgid.
//
// NO OFFLINE GATE, deliberately: AppShell.slint mounts both routes while
// offline (:456-470) — neither is in `qobuz-view-blocked` (:112-136) — because
// both read a LOCAL store. Rendering QbzOfflinePlaceholder here would hide
// data that is perfectly available.
//
// NAV BUTTONS: back/forward live in the shell header, so the title starts at
// x: 48 (see DiscoverBrowseView.qml's note).

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    property bool kioskHost: false

    QbzTheme { id: theme }

    readonly property var doc: {
        try {
            return JSON.parse(QbzHome.playHistoryJson)
        } catch (e) {
            return {}
        }
    }
    // The document is the source of truth; `currentView` only covers the
    // frame before the first publish lands.
    readonly property string mode: doc.mode
        || (QbzShell.currentView === "mostplayedalbums" ? "mostPlayed" : "recent")
    readonly property bool mostPlayed: mode === "mostPlayed"
    readonly property var items: doc.items || []

    readonly property bool ambientOn: theme.ambientOn
    // RecentAlbumsView.slint:31 is ambient-aware; MostPlayedAlbumsView
    // .slint:26 is plain surface-main.
    color: (!mostPlayed && ambientOn) ? "transparent" : theme.surfaceMain
    radius: 12

    Column {
        anchors.fill: parent
        spacing: 0

        // --- Fixed 56px header --------------------------------------------
        Item {
            width: parent.width
            height: root.kioskHost ? 64 : 56

            // Unconditional surface-main on BOTH pages (:43 / :34) — these
            // two never take the ambient bar alpha the browse pages do.
            Rectangle {
                anchors.fill: parent
                // Top-rounded: full-bleed at y=0 of the content pane. The fill
                // stays UNCONDITIONAL per the note above (that is reference
                // parity), but the corner is not a colour question — in Slint
                // the pane's `clip` follows its radius and cuts this band;
                // Qt's clip is a rectangular scissor, so the band squares the
                // pane unless it rounds itself. Invisible with the background
                // off, where the view root paints the same colour beneath.
                topLeftRadius: theme.radiusMd
                topRightRadius: theme.radiusMd
                color: theme.surfaceMain
            }

            Text {
                x: 48
                y: (root.kioskHost ? 32 : 25) - height / 2
                width: Math.max(0, parent.width - 48 - 32 - (root.mostPlayed ? 240 + 16 : 0))
                text: root.mostPlayed
                    ? QbzSession.tr("Most Played Albums", QbzSession.trRev)
                    : QbzSession.tr("Recently Played Albums", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: theme.fontSection
                font.weight: theme.weightBold
                elide: Text.ElideRight
            }

            // Most Played only: a 240x34 title/artist filter. No clear-X in
            // the .slint (:59-104), so the shared search arm is not used here.
            Rectangle {
                visible: root.mostPlayed
                x: parent.width - width - 32
                y: (root.kioskHost ? 32 : 25) - height / 2
                width: 240
                height: root.kioskHost ? 44 : 34
                radius: 6
                border.width: 1
                border.color: theme.borderSubtle
                color: theme.surfaceElevated

                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 8
                    QbzIcon {
                        name: "search"
                        width: 14
                        height: 14
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: "muted"
                    }
                    Item {
                        width: parent.width - 14 - 8
                        height: parent.height
                        clip: true
                        TextInput {
                            QbzTextEditMenu { }
                            id: mpSearch
                            anchors.fill: parent
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            verticalAlignment: Text.AlignVCenter
                            selectByMouse: true
                            onTextEdited: QbzHome.mostPlayedFilter(text)
                            // The opener resets the box in the same hop that
                            // raises `loading`; re-seed while not editing.
                            Binding {
                                target: mpSearch
                                property: "text"
                                value: root.doc.search || ""
                                when: !mpSearch.activeFocus
                            }
                        }
                        Text {
                            visible: mpSearch.text === ""
                            anchors.fill: parent
                            text: QbzSession.tr("Search…", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: theme.fontBody
                            verticalAlignment: Text.AlignVCenter
                        }
                    }
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

                    // Spinner + "Loading…" — identical on both pages.
                    Column {
                        visible: QbzHome.playHistoryLoading
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

                    Text {
                        visible: !QbzHome.playHistoryLoading && root.items.length === 0
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: QbzSession.tr("Albums you play will appear here.", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 14
                    }

                    AlbumCollection {
                kioskHost: root.kioskHost
                        visible: !QbzHome.playHistoryLoading
                        width: parent.width - 64
                        albums: root.items
                        viewMode: "grid"
                        cardWidth: 200
                        // The 20px difference IS the "{} plays" line.
                        cardHeight: root.mostPlayed ? 286 : 266
                        cardGap: 24
                        showPlays: root.mostPlayed
                        flick: flick
                        contentOffset: root.kioskHost ? y : (8)
                    }
                }
            }

            // Back/forward scroll memory (controls/ScrollMemory.qml): reports
            // this container's offset while it is the live page, and restores it
            // when a back/forward step arms this route.
            ScrollMemory { target: flick; scope: QbzShell.currentView }
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
