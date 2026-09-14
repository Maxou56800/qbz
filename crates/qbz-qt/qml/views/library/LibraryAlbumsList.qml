// LibraryAlbumsList — the Library > Albums tab in LIST mode
// (FavoritesView.slint:1762 `AlbumCollectionView { view-mode: … }` with
// `albums-view-mode == "list"`), plus its multi-select bulk bar (:1734).
//
// REUSE, not a fork: the row IS views/AlbumListRow.qml and the column header
// IS views/AlbumListHeader.qml — the pair built for Discover Browse / Label
// Releases. Two properties were added to the row rather than copying it
// (`artSource`, because this feed is id-keyed through LibraryView's artMap
// instead of carrying a baked `artPath`; and the `selectMode`/`checked`
// /`toggleSelect` trio the reference's LIST-only multi-select needs). Both
// default to the previous behaviour, so the two catalog call sites are
// untouched.
//
// The A-Z jump strip is NOT here: one strip serves grid AND list (and the
// tracks list), so it lives in views/LibraryView.qml next to the surfaces it
// scrolls.

import QtQuick
import com.blitzfc.qbz
import ".."
import "../../controls"
import "../../theme"

Item {
    id: root

    /// The LibraryView root (artMap, the selection map, the window reporter).
    property var view: null
    /// The Library's keyed model (views/LibraryView.qml `feedRows`), shared by
    /// every body so a row leaving animates instead of rebuilding the list.
    property var rowsModel: null
    /// For the model's synchronous reset layout.
    readonly property alias listView: list

    QbzTheme { id: theme }

    readonly property int rowH: 64
    readonly property int rowGap: 4

    /// A-Z / index jump target for the host's strip.
    function positionAt(index) {
        list.positionViewAtIndex(index, ListView.Beginning)
    }
    /// Re-report the artwork window (the host calls it after a reconcile).
    function report() { list.report() }

    Column {
        anchors.fill: parent
        spacing: 8

        QbzMultiSelectBar {
            visible: root.view.albumsMultiSelect
            width: parent.width
            selectedCount: root.view.albumsSelectedCount
            // FavoritesView.slint:1737-1743, verbatim. "Make available
            // offline" is NOT in the reference's ALBUM bar (only the tracks
            // one), so nothing is omitted here.
            actions: [
                { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                { "id": "play-next", "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "danger": false, "needsSelection": true },
                { "id": "play-later", "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "danger": false, "needsSelection": true },
                { "id": "queue", "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "danger": false, "needsSelection": true },
                // The .slint asks for `heart-crack`; this port's icon set has
                // no such glyph (checked: assets/icons/*/heart*.svg is heart
                // and heart-filled only), and a missing name renders NOTHING.
                // "heart" + danger:true is what views/local/LocalTracksTab.qml
                // already ships for the same action, so the two bars match.
                { "id": "remove-selected", "label": QbzSession.tr("Remove from Library", QbzSession.trRev), "icon": "heart", "danger": true, "needsSelection": true },
                { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true },
            ]
            onAction: function (id) { root.view.albumsBulkAction(id) }
        }

        Item {
            width: parent.width
            height: parent.height - (root.view.albumsMultiSelect ? 52 : 0)

            AlbumListHeader {
                id: header
                width: parent.width
                selectMode: root.view.albumsMultiSelect
            }

            ListView {
                id: list
                anchors.top: header.bottom
                anchors.topMargin: 4
                anchors.left: parent.left
                anchors.right: parent.right
                // Height-gated like the host's grid: a hidden ListView with a
                // model keeps refilling its viewport, and this one shares the
                // Library's model with every other body.
                height: root.visible ? parent.height - header.height - 4 : 0
                // No strip inset here: the A-Z strip lives in LibraryView and
                // the HOST already widened this component's right margin for
                // it. Insetting again would double-count it.
                clip: true
                spacing: root.rowGap
                cacheBuffer: 64 * 8
                reuseItems: true
                boundsBehavior: Flickable.StopAtBounds
                model: root.rowsModel
                add: QbzRowAdd { enabled: root.rowsModel !== null && root.rowsModel.animate }
                remove: QbzRowRemove { enabled: root.rowsModel !== null && root.rowsModel.animate }
                move: QbzRowDisplaced { enabled: root.rowsModel !== null && root.rowsModel.animate }
                displaced: QbzRowDisplaced { enabled: root.rowsModel !== null && root.rowsModel.animate }

                // Windowed artwork, same contract as the grid: never read
                // `list.model` back off the view (views/LibraryView.qml
                // documents the SIGSEGV), always the array from the host.
                //
                // Gated on visibility like the host's own bodies
                // (LibraryView.gridWindowReport / listWindowReport): `artMap`
                // is ONE map and a report PRUNES outside its band, while this
                // body keeps firing hidden (`anchors.fill` turns any window
                // resize into onHeightChanged). Ungated it evicted whichever
                // body is actually on screen, the Artists sidepanel rail above
                // all, whose band is NOT a feed band. Offsets count from
                // `originY`, which rows leaving above the window move.
                function report() {
                    if (!list.visible || root.rowsModel === null) return
                    var stride = root.rowH + root.rowGap
                    var scrollY = list.contentY - list.originY
                    var first = Math.max(0, Math.floor(scrollY / stride) - 4)
                    var last = Math.ceil((scrollY + list.height) / stride) + 4
                    root.view.queueWindowReport(first,
                        Math.min(root.rowsModel.published.length - 1, last))
                }
                onContentYChanged: list.report()
                onModelChanged: list.report()
                onHeightChanged: list.report()
                onVisibleChanged: list.report()
                Component.onCompleted: list.report()

                delegate: AlbumListRow {
                    required property string rowKey
                    required property int rowRev
                    required property int index
                    readonly property var modelData: root.rowsModel.row(rowKey, rowRev)
                    ListView.onReused: { opacity = 1; scale = 1 }
                    width: list.width
                    item: modelData
                    rowIndex: index
                    artSource: root.view.artMap[modelData.artKey] || ""
                    selectMode: root.view.albumsMultiSelect
                    checked: root.view.albumsSelected[modelData.id] === true
                    onToggleSelect: function (mods) { root.view.toggleAlbumSelected(modelData.id, mods) }
                }
            }

            QbzScrollBar {
                anchors.right: parent.right
                anchors.rightMargin: 4
                anchors.top: list.top
                anchors.bottom: list.bottom
                target: list
                visible: list.contentHeight > list.height
            }
        }
    }
}
