// The Library grid over QbzKeyedModel, with the REAL FeedGridCell delegate and
// LibraryView's key and transitions: an album leaving (a republished feed with
// all-new row objects, exactly what publish_library_document produces) must
// keep every other card's delegate and its decoded cover — no rebuild, no
// covers fading back in — and keep the page where it was.
import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/controls"
import "../../crates/qbz-qt/qml/views/library" as Library

Item {
    id: root
    width: 880; height: 800

    property var feed: []
    property int created: 0

    QtObject {
        id: host
        property var artMap: ({})
        property bool showSourceBadges: false
        property string activeTab: "albums"
        property bool skelPhase: false
        function askRemoveReleaseFavorites(item) {}
        function isLocalFeedItem(item) { return false }
    }

    function album(i) {
        return { "kind": "album", "id": String(1000 + i), "artKey": "cover" + i,
                 "title": "Album " + i, "artist": "Artist", "artistId": "1",
                 "genre": "", "year": "2000", "qualityTier": "", "source": "qobuz",
                 "group": "favorites", "imageUrl": "fixture", "isFavorite": true,
                 "isPinned": false, "_membershipKey": "album:" + (1000 + i),
                 "_feedOrder": i }
    }
    // A republished document: same data, all-new objects, renumbered order.
    function publish(ids) {
        var out = []
        for (var i = 0; i < ids.length; i++) {
            var row = album(ids[i])
            row._feedOrder = i
            out.push(row)
        }
        root.feed = out
    }

    QbzKeyedModel {
        id: feedRows
        rows: root.feed
        keyOf: function (row) {
            if (row.kind === "group-header")
                return row.id
            return (row._membershipKey || (row.kind + ":" + row.id))
                + (host.activeTab === "all" ? "|" + (row.group || "") : "")
        }
        scope: "albums"
        ignoredKeys: ["_feedOrder"]
        views: [grid]
    }

    GridView {
        id: grid
        width: 880; height: 800
        cellWidth: 220; cellHeight: 266
        cacheBuffer: 266 * 2
        reuseItems: true
        clip: true
        model: feedRows
        add: QbzRowAdd { enabled: feedRows.animate; grow: 0.94 }
        remove: QbzRowRemove { enabled: feedRows.animate; shrink: 0.92 }
        move: QbzRowDisplaced { enabled: feedRows.animate }
        displaced: QbzRowDisplaced { enabled: feedRows.animate }
        delegate: Library.FeedGridCell {
            required property string rowKey
            required property int rowRev
            required property int index
            readonly property var modelData: feedRows.row(rowKey, rowRev)
            view: host
            item: modelData
            Component.onCompleted: root.created++
        }
    }

    TestCase {
        name: "LibraryKeyed"
        when: windowShown

        function card(delegate) {
            for (var i = 0; i < delegate.children.length; i++) {
                var child = delegate.children[i]
                if (child.item && child.item.artworkReady !== undefined)
                    return child.item
            }
            return null
        }
        // GridView fills its cache buffer a few cells per frame, so creation
        // counts are only comparable once they stop moving.
        function settleCreation() {
            var last = -1
            var stable = 0
            while (stable < 3) {
                wait(120)
                if (root.created === last)
                    stable++
                else
                    stable = 0
                last = root.created
            }
        }
        function delegateFor(key) {
            var i = feedRows.indexOfKey(key)
            return i < 0 ? null : grid.itemAtIndex(i)
        }

        function test_album_leaving_keeps_cards_covers_and_page() {
            var ids = []
            for (var i = 0; i < 120; i++)
                ids.push(i)
            var paths = ({})
            for (i = 0; i < 120; i++)
                paths["cover" + i] = Qt.resolvedUrl("fixtures/red.ppm").toString()
            host.artMap = paths
            root.publish(ids)
            wait(0)
            waitForRendering(grid)
            // Scroll to the fourth row of cards and let its covers decode.
            grid.contentY = grid.originY + 3 * 266
            waitForRendering(grid)
            var watched = ["album:1013", "album:1014", "album:1015", "album:1019"]
            for (var w = 0; w < watched.length; w++) {
                var key = watched[w]
                tryVerify(function () {
                    var d = delegateFor(key)
                    return d !== null && card(d) !== null && card(d).artworkReady
                }, 3000, "cover decoded for " + key)
            }
            settleCreation()
            var before = ({})
            for (w = 0; w < watched.length; w++)
                before[watched[w]] = delegateFor(watched[w])
            var madeBefore = root.created
            var offsetBefore = grid.contentY - grid.originY

            // Album 1013 (on screen) leaves: the feed is republished.
            root.publish(ids.filter(function (id) { return id !== 13 }))
            wait(0)
            wait(400)
            waitForRendering(grid)
            settleCreation()

            compare(feedRows.indexOfKey("album:1013"), -1)
            var survivors = ["album:1014", "album:1015", "album:1019"]
            for (w = 0; w < survivors.length; w++) {
                var d = delegateFor(survivors[w])
                verify(d === before[survivors[w]], "same delegate for " + survivors[w])
                verify(card(d).artworkReady, "cover still decoded for " + survivors[w])
                compare(d.opacity, 1)
            }
            // One card left, so at most one row's worth of cells is new; a
            // rebuild creates every visible and buffered cell again (20+).
            verify(root.created - madeBefore <= 4,
                   "no rebuild: " + (root.created - madeBefore) + " cells created")
            compare(grid.contentY - grid.originY, offsetBefore, "the page stays where it was")
        }
    }
}
