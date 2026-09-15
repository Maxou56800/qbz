// Track-row click policy (issue #790): on every desktop track row a single
// click on the body does NOTHING, a double click anywhere that is not a
// control plays exactly once, and the play disc plays on its first click.
// The REAL row components are clicked through the window, so whichever
// MouseArea really sits on top at a point is the one that answers. Select
// mode, dead rows and the drag gesture must keep their own behaviour, and the
// kiosk (touch, no hover disc) keeps tap-to-play.
import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/rows"
import "../../crates/qbz-qt/qml/views/local" as Local
import "../../crates/qbz-qt/qml/views/purchases" as Purchases
import "../../crates/qbz-qt/qml/views/library" as Library

Item {
    id: root
    width: 1200; height: 900

    function track(i) {
        return { "id": "t" + i, "title": "Track " + i, "artist": "Artist " + i,
                 "artistId": "a" + i, "album": "Album " + i, "albumId": "al" + i,
                 "duration": "3:00", "qualityTier": "", "qualityDetail": "" }
    }

    property var plays: []
    property var toggles: []
    function played(tag) { root.plays = root.plays.concat([tag]) }

    TrackRow {
        id: desktopRow
        y: 0
        width: 1000
        height: 50
        item: root.track(1)
        number: 1
        artistLink: true
        showAlbum: true
        onPlayRequested: root.played("desktop")
        onToggleSelect: function (mods) { root.toggles = root.toggles.concat([mods]) }
    }

    TrackRow {
        id: kioskRow
        y: 60
        width: 1000
        height: 64
        kioskHost: true
        item: root.track(2)
        number: 2
        onPlayRequested: root.played("kiosk")
    }

    Local.LocalTrackRow {
        id: localRow
        y: 140
        width: 1000
        item: ({ "id": "11", "title": "Local 1", "artist": "Local Artist", "album": "Local Album",
                 "albumId": "la1", "duration": "4:00", "source": "local", "qualityTier": "",
                 "qualityDetail": "" })
        number: 1
        onPlayRequested: root.played("local")
    }

    Purchases.PurchaseTrackRow {
        id: purchaseRow
        y: 210
        width: 1000
        track: ({ "id": "p1", "title": "Bought 1", "artist": "Seller", "album": "Receipt",
                  "duration": 200, "streamable": true })
        onPlayRequested: root.played("purchase")
    }

    QtObject {
        id: feedView
        property var artMap: ({})
        property bool skelPhase: false
        property bool showSourceBadges: false
        function playTrackInContext(id) { root.played("feed:" + id) }
        function trackAction(item, action) {}
        function trackMenuModel(item) { return [] }
    }
    Library.FeedListRow {
        id: feedTrack
        y: 290
        width: 1000
        view: feedView
        item: ({ "kind": "track", "id": "f1", "title": "Feed track", "subtitle": "Feed artist",
                 "artistId": "fa1", "source": "qobuz", "artKey": "" })
    }
    Library.FeedListRow {
        id: feedAlbum
        y: 350
        width: 1000
        view: feedView
        item: ({ "kind": "album", "id": "fal1", "title": "Feed album", "subtitle": "Feed artist",
                 "artistId": "fa1", "source": "qobuz", "artKey": "" })
    }

    TestCase {
        name: "TrackRowClick"
        when: windowShown

        function find(item, pred) {
            if (!item)
                return null
            if (pred(item))
                return item
            var kids = item.children || []
            for (var i = 0; i < kids.length; i++) {
                var hit = find(kids[i], pred)
                if (hit)
                    return hit
            }
            return null
        }
        function shown(item) {
            for (var it = item; it; it = it.parent)
                if (!it.visible)
                    return false
            return true
        }
        function textItem(host, text) {
            var t = find(host, function (it) {
                return it.text === text && it.contentWidth !== undefined && shown(it)
            })
            verify(t !== null, "text '" + text + "' is drawn")
            return t
        }
        function at(item, x, y) { return item.mapToItem(root, x, y) }
        function click(item, x, y) {
            var p = at(item, x, y)
            mouseClick(root, p.x, p.y, Qt.LeftButton)
            wait(20)
        }
        function doubleClick(item, x, y) {
            var p = at(item, x, y)
            mouseDoubleClickSequence(root, p.x, p.y, Qt.LeftButton)
            wait(20)
        }
        // Centre of a drawn text: a point with nothing but the body under it
        // when the text is plain (duration, title).
        function textCentre(host, text) {
            var t = textItem(host, text)
            return t.mapToItem(host, t.width / 2, t.height / 2)
        }
        // The far end of a text's box, past the drawn glyphs: whitespace.
        function textTail(host, text) {
            var t = textItem(host, text)
            verify(t.width - t.contentWidth > 20, "'" + text + "' leaves whitespace")
            return t.mapToItem(host, t.width - 6, t.height / 2)
        }
        function textHead(host, text) {
            var t = textItem(host, text)
            return t.mapToItem(host, 4, t.height / 2)
        }

        function init() {
            root.plays = []
            root.toggles = []
            QbzArtist.opened = []
            QbzAlbum.opened = []
            QbzShell.dragStarts = 0
            desktopRow.selectMode = false
            desktopRow.playBlocked = false
            desktopRow.item = root.track(1)
            wait(450) // outside the row's double-click window and play debounce
        }

        function test_desktop_single_click_on_body_does_nothing() {
            var p = textCentre(desktopRow, "3:00")
            click(desktopRow, p.x, p.y)
            p = textCentre(desktopRow, "Track 1")
            click(desktopRow, p.x, p.y)
            compare(root.plays, [])
        }

        function test_desktop_double_click_plays_once() {
            var p = textCentre(desktopRow, "3:00")
            doubleClick(desktopRow, p.x, p.y)
            compare(root.plays, ["desktop"])
        }

        function test_desktop_body_cursor_does_not_promise_a_click() {
            var body = find(desktopRow, function (it) { return it.z === -1 && it.cursorShape !== undefined })
            verify(body !== null)
            compare(body.cursorShape, Qt.ArrowCursor)
        }

        function test_play_disc_plays_on_first_click() {
            click(desktopRow, 20, desktopRow.height / 2)
            compare(root.plays, ["desktop"])
        }

        function test_artist_name_navigates_and_its_whitespace_plays() {
            var p = textHead(desktopRow, "Artist 1")
            click(desktopRow, p.x, p.y)
            compare(QbzArtist.opened, ["a1"])
            compare(root.plays, [])
            wait(450)
            QbzArtist.opened = []
            p = textTail(desktopRow, "Artist 1")
            click(desktopRow, p.x, p.y)
            compare(QbzArtist.opened, [], "whitespace beside the name is not a link")
            wait(450)
            doubleClick(desktopRow, p.x, p.y)
            compare(root.plays, ["desktop"])
            compare(QbzArtist.opened, [])
        }

        function test_album_name_navigates_and_its_whitespace_plays() {
            var p = textHead(desktopRow, "Album 1")
            click(desktopRow, p.x, p.y)
            compare(QbzAlbum.opened, ["al1"])
            wait(450)
            QbzAlbum.opened = []
            p = textTail(desktopRow, "Album 1")
            doubleClick(desktopRow, p.x, p.y)
            compare(root.plays, ["desktop"])
            compare(QbzAlbum.opened, [])
        }

        function test_select_mode_click_toggles_and_double_click_never_plays() {
            desktopRow.selectMode = true
            wait(20)
            var p = textCentre(desktopRow, "3:00")
            click(desktopRow, p.x, p.y)
            compare(root.toggles.length, 1)
            wait(450)
            root.toggles = []
            click(desktopRow, p.x, p.y, Qt.ShiftModifier)
            wait(450)
            doubleClick(desktopRow, p.x, p.y)
            compare(root.plays, [])
        }

        function test_select_mode_click_keeps_modifiers() {
            desktopRow.selectMode = true
            wait(20)
            var p = at(desktopRow, textCentre(desktopRow, "3:00").x, textCentre(desktopRow, "3:00").y)
            mouseClick(root, p.x, p.y, Qt.LeftButton, Qt.ShiftModifier)
            wait(20)
            compare(root.toggles, [Qt.ShiftModifier])
        }

        function test_dead_and_blocked_rows_do_not_play_on_double_click() {
            var dead = root.track(1)
            dead.qobuzUnavailable = true
            desktopRow.item = dead
            wait(20)
            var p = textCentre(desktopRow, "3:00")
            doubleClick(desktopRow, p.x, p.y)
            compare(root.plays, [], "pulled row")
            desktopRow.item = root.track(1)
            desktopRow.playBlocked = true
            wait(450)
            doubleClick(desktopRow, p.x, p.y)
            compare(root.plays, [], "play-blocked row")
        }

        function test_horizontal_drag_starts_a_drag_and_never_plays() {
            var p = at(desktopRow, textCentre(desktopRow, "Track 1").x, textCentre(desktopRow, "Track 1").y)
            mousePress(root, p.x, p.y, Qt.LeftButton)
            for (var dx = 4; dx <= 40; dx += 4)
                mouseMove(root, p.x + dx, p.y + 1)
            mouseRelease(root, p.x + 40, p.y + 1, Qt.LeftButton)
            wait(20)
            compare(QbzShell.dragStarts, 1)
            compare(root.plays, [])
        }

        function test_kiosk_row_keeps_tap_to_play() {
            var p = textCentre(kioskRow, "3:00")
            click(kioskRow, p.x, p.y)
            compare(root.plays, ["kiosk"])
            wait(450)
            root.plays = []
            doubleClick(kioskRow, p.x, p.y)
            compare(root.plays, ["kiosk"], "a double tap is still one play")
        }

        function test_local_row_single_click_does_nothing_and_gutter_double_click_plays() {
            var p = textCentre(localRow, "4:00")
            click(localRow, p.x, p.y)
            compare(root.plays, [])
            wait(450)
            doubleClick(localRow, p.x, p.y)
            compare(root.plays, ["local"])
            wait(450)
            root.plays = []
            // The source-glyph gutter the wrapper draws outside the shared row.
            doubleClick(localRow, localRow.width - 13, localRow.height / 2)
            compare(root.plays, ["local"])
        }

        function test_purchase_row_single_click_does_nothing_double_click_plays() {
            var p = textCentre(purchaseRow, "Bought 1")
            click(purchaseRow, p.x, p.y)
            compare(root.plays, [])
            wait(450)
            doubleClick(purchaseRow, p.x, p.y)
            compare(root.plays, ["purchase"])
        }

        function test_feed_track_row_single_click_does_nothing_double_click_plays() {
            var p = textCentre(feedTrack, "Feed track")
            click(feedTrack, p.x, p.y)
            p = textCentre(feedTrack, "Track")
            click(feedTrack, p.x, p.y)
            compare(root.plays, [])
            wait(450)
            doubleClick(feedTrack, p.x, p.y)
            compare(root.plays, ["feed:f1"])
            wait(450)
            root.plays = []
            p = textCentre(feedTrack, "Feed track")
            doubleClick(feedTrack, p.x, p.y)
            compare(root.plays, ["feed:f1"])
        }

        function test_feed_album_row_still_opens_on_click() {
            var p = textCentre(feedAlbum, "Feed album")
            click(feedAlbum, p.x, p.y)
            compare(QbzAlbum.opened, ["fal1"])
            compare(root.plays, [])
        }
    }
}
