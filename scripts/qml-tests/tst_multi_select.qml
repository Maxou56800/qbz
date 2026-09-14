// Excel-style multi-select regression suite: the shared selection rule
// (controls/SelectionModel.qml) and the REAL row, card, checkbox and rail
// components of the select-mode surfaces. Every click target a select-mode
// surface offers must deliver Shift (a range from the anchor) and Ctrl (one
// more row) to the selection; a target that swallows the modifier, or that
// navigates away instead of selecting, is exactly the regression this guards.
import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/controls"
import "../../crates/qbz-qt/qml/rows"
import "../../crates/qbz-qt/qml/cards"
import "../../crates/qbz-qt/qml/views" as Views
import "../../crates/qbz-qt/qml/views/local" as Local
import "../../crates/qbz-qt/qml/views/myqbz" as MyQbz
import "../../crates/qbz-qt/qml/settings" as Settings

Item {
    id: root
    width: 1200; height: 1400

    function track(i) {
        return { "id": "t" + i, "title": "Track " + i, "artist": "Artist " + i,
                 "artistId": "a" + i, "album": "Album " + i, "albumId": "al" + i,
                 "duration": "3:00", "qualityTier": "", "qualityDetail": "" }
    }
    function album(i) {
        return { "id": "b" + i, "title": "Record " + i, "artist": "Band " + i,
                 "artistId": "band" + i, "label": "Label " + i, "labelId": "l" + i,
                 "year": "2001", "qualityTier": "", "qualityDetail": "" }
    }
    function range(n, make) {
        var out = []
        for (var i = 0; i < n; i++)
            out.push(make(i))
        return out
    }

    // ---- the track list (AlbumView / PlaylistView wiring) -------------------
    property var tracks: range(8, track)
    property var trackSelected: ({})
    SelectionModel { id: trackSel }
    function toggleTrack(id, mods) {
        root.trackSelected = trackSel.next(root.trackSelected, id, root.tracks,
                                           mods === undefined ? Qt.NoModifier : mods)
    }

    Column {
        id: trackColumn
        width: 1000
        Repeater {
            id: trackRepeater
            model: root.tracks
            delegate: TrackRow {
                required property var modelData
                required property int index
                width: 1000
                item: modelData
                number: index + 1
                artistLink: true
                showAlbum: true
                selectMode: true
                checked: root.trackSelected[modelData.id] === true
                onToggleSelect: function (mods) { root.toggleTrack(modelData.id, mods) }
            }
        }
    }

    // ---- the album list (Library albums list wiring) ------------------------
    property var albums: range(6, album)
    property var albumSelected: ({})
    SelectionModel { id: albumSel }
    function toggleAlbum(id, mods) {
        root.albumSelected = albumSel.next(root.albumSelected, id, root.albums,
                                           mods === undefined ? Qt.NoModifier : mods)
    }

    Column {
        id: albumColumn
        y: 420
        width: 1000
        Repeater {
            id: albumRepeater
            model: root.albums
            delegate: Views.AlbumListRow {
                required property var modelData
                required property int index
                width: 1000
                item: modelData
                rowIndex: index
                selectMode: true
                checked: root.albumSelected[modelData.id] === true
                onToggleSelect: function (mods) { root.toggleAlbum(modelData.id, mods) }
            }
        }
    }

    // ---- one album card and the two checkbox controls ------------------------
    property var cardMods: []
    AlbumCard {
        id: card
        x: 1000; y: 0
        albumId: "card"
        title: "Card title"
        artist: "Card artist"
        artistId: "card-artist"
        selectMode: true
        onSelectToggled: function (mods) { root.cardMods = root.cardMods.concat([mods]) }
    }

    property var checkboxMods: []
    QbzCheckbox {
        id: checkbox
        x: 1000; y: 320
        onToggled: function (mods) { root.checkboxMods = root.checkboxMods.concat([mods]) }
    }
    property var selectCheckMods: []
    Local.SelectCheck {
        id: selectCheck
        x: 1040; y: 320
        onToggled: function (mods) { root.selectCheckMods = root.selectCheckMods.concat([mods]) }
    }

    // ---- a My QBZ collection row (selection lives in Rust) ----------------------
    MyQbz.MyQbzDetailRow {
        id: myqbzRow
        x: 0; y: 820
        width: 1000
        selectMode: true
        item: ({ "position": 3, "itemType": "album", "source": "qobuz",
                 "sourceItemId": "alb3", "title": "Collected record", "subtitle": "Collected band",
                 "subtitleIsLink": true, "artistId": "cb" })
    }

    // ---- the Local Library folder rail (selection lives in Rust) ----------------
    QtObject {
        id: railView
        property bool treeSelectMode: true
        property string selectedFolder: ""
        property string treeSearch: ""
        property bool skelPhase: false
        property var opened: []
        property var tree: [
            { "path": "/m/a", "segment": "a", "depth": 0, "isFolder": true, "canExpand": true },
            { "path": "/m/a/1.flac", "segment": "1.flac", "depth": 1, "isFolder": false },
            { "path": "/m/b", "segment": "b", "depth": 0, "isFolder": true, "canExpand": true },
            { "path": "/m/c", "segment": "c", "depth": 0, "isFolder": true, "canExpand": true },
            { "path": "/m/d", "segment": "d", "depth": 0, "isFolder": true, "canExpand": true }
        ]
        function toggleTreeSelectMode() { treeSelectMode = !treeSelectMode }
        function selectFolder(path) { opened = opened.concat([path]) }
    }
    Local.LocalTreeRail {
        id: rail
        x: 1000; y: 400
        width: 200; height: 400
        view: railView
    }

    // ---- Settings > Local Library folders ---------------------------------------
    Settings.LibraryFolderTable {
        id: folderTable
        x: 0; y: 880
        width: 1200
        lib: ({ "folders": [1, 2, 3, 4, 5].map(function (n) {
            return { "id": n, "displayName": "Music " + n, "path": "/music/" + n,
                     "isNetwork": false, "enabled": true, "accessible": true,
                     "status": "active", "lastScan": 0 }
        }) })
    }

    TestCase {
        name: "MultiSelect"
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
        // Clicks go through the window at the item's position, so whichever
        // MouseArea really sits on top there receives them — the point of
        // using the real components.
        function clickOn(item, x, y, mods) {
            var p = item.mapToItem(root, x, y)
            mouseClick(root, p.x, p.y, Qt.LeftButton, mods === undefined ? Qt.NoModifier : mods)
            wait(20)
        }
        function selectedIds(map) {
            return Object.keys(map).sort().join(",")
        }
        function trackRow(i) { return trackRepeater.itemAt(i) }
        function albumRow(i) { return albumRepeater.itemAt(i) }
        function disc(row) {
            var d = find(row, function (it) {
                return it.width === 14 && it.height === 14 && it.hoverEnabled !== undefined
                    && it.cursorShape !== undefined && shown(it)
            })
            verify(d !== null, "the select-mode disc is drawn")
            return d
        }

        function init() {
            root.trackSelected = ({})
            trackSel.anchorId = ""
            root.albumSelected = ({})
            albumSel.anchorId = ""
            root.cardMods = []
            root.checkboxMods = []
            root.selectCheckMods = []
            QbzArtist.opened = []
            QbzAlbum.opened = []
            QbzHome.labels = []
            QbzMyQbz.selectCalls = []
            QbzMyQbz.opened = []
            QbzMyQbz.played = []
            QbzLocal.treeCalls = []
            railView.opened = []
            railView.treeSelectMode = true
            folderTable.selectedIds = []
        }

        // A point of the row with nothing drawn over it but the body: the
        // duration cell.
        function bodyPoint(row) {
            var duration = textItem(row, "3:00")
            return duration.mapToItem(row, duration.width / 2, duration.height / 2)
        }
        function clickBody(row, mods) {
            var p = bodyPoint(row)
            clickOn(row, p.x, p.y, mods)
        }

        function test_row_body_shift_and_ctrl() {
            clickBody(trackRow(1))
            clickBody(trackRow(4), Qt.ShiftModifier)
            compare(selectedIds(root.trackSelected), "t1,t2,t3,t4")
            clickBody(trackRow(6), Qt.ControlModifier)
            compare(selectedIds(root.trackSelected), "t1,t2,t3,t4,t6")
        }

        function test_checkbox_disc_shift_and_ctrl() {
            clickOn(disc(trackRow(0)), 7, 7)
            clickOn(disc(trackRow(3)), 7, 7, Qt.ShiftModifier)
            compare(selectedIds(root.trackSelected), "t0,t1,t2,t3",
                    "Shift on the disc selects the range")
            clickOn(disc(trackRow(5)), 7, 7, Qt.ControlModifier)
            clickOn(disc(trackRow(7)), 7, 7, Qt.ControlModifier | Qt.ShiftModifier)
            compare(selectedIds(root.trackSelected), "t0,t1,t2,t3,t5,t6,t7",
                    "Ctrl adds a row and Ctrl+Shift adds a second group")
        }

        function test_artist_link_selects_in_select_mode() {
            clickBody(trackRow(2))
            var name = textItem(trackRow(5), "Artist 5")
            clickOn(name, 10, name.height / 2, Qt.ShiftModifier)
            compare(QbzArtist.opened.length, 0, "no artist page opens in select mode")
            compare(selectedIds(root.trackSelected), "t2,t3,t4,t5")
        }

        function test_album_link_selects_in_select_mode() {
            var name = textItem(trackRow(6), "Album 6")
            clickOn(name, 10, name.height / 2)
            compare(QbzAlbum.opened.length, 0, "no album page opens in select mode")
            compare(selectedIds(root.trackSelected), "t6")
        }

        function test_album_row_checkbox_shift_range() {
            var first = find(albumRow(0), function (it) {
                return it.checked !== undefined && it.width === 18 && it.height === 18 && shown(it)
            })
            var fourth = find(albumRow(3), function (it) {
                return it.checked !== undefined && it.width === 18 && it.height === 18 && shown(it)
            })
            verify(first !== null && fourth !== null, "the album row checkboxes are drawn")
            clickOn(first, 9, 9)
            clickOn(fourth, 9, 9, Qt.ShiftModifier)
            compare(selectedIds(root.albumSelected), "b0,b1,b2,b3")
        }

        function test_album_row_links_select_in_select_mode() {
            clickOn(albumRow(1), 600, 32)
            var band = textItem(albumRow(4), "Band 4")
            clickOn(band, 10, band.height / 2, Qt.ShiftModifier)
            compare(QbzArtist.opened.length, 0, "no artist page opens in select mode")
            var label = textItem(albumRow(5), "Label 5")
            clickOn(label, 10, label.height / 2, Qt.ControlModifier)
            compare(QbzHome.labels.length, 0, "no label page opens in select mode")
            compare(selectedIds(root.albumSelected), "b1,b2,b3,b4,b5")
        }

        function test_album_card_forwards_modifiers() {
            clickOn(card, 100, 100, Qt.ShiftModifier)
            var title = textItem(card, "Card title")
            clickOn(title, 10, title.height / 2, Qt.ControlModifier)
            var artist = textItem(card, "Card artist")
            clickOn(artist, 10, artist.height / 2, Qt.ShiftModifier)
            compare(QbzArtist.opened.length, 0, "no artist page opens in select mode")
            compare(root.cardMods.length, 3)
            verify((root.cardMods[0] & Qt.ShiftModifier) !== 0, "artwork carries Shift")
            verify((root.cardMods[1] & Qt.ControlModifier) !== 0, "title carries Ctrl")
            verify((root.cardMods[2] & Qt.ShiftModifier) !== 0, "artist line carries Shift")
        }

        function test_checkbox_controls_forward_modifiers() {
            clickOn(checkbox, 9, 9, Qt.ShiftModifier)
            clickOn(selectCheck, 6, 6, Qt.ControlModifier)
            compare(root.checkboxMods.length, 1)
            verify((root.checkboxMods[0] & Qt.ShiftModifier) !== 0, "QbzCheckbox carries Shift")
            compare(root.selectCheckMods.length, 1)
            verify((root.selectCheckMods[0] & Qt.ControlModifier) !== 0, "SelectCheck carries Ctrl")
            checkbox.forceActiveFocus()
            keyClick(Qt.Key_Space)
            compare(root.checkboxMods.length, 2)
            compare(root.checkboxMods[1] & Qt.ShiftModifier, 0, "the keyboard toggle is a plain toggle")
        }

        function test_myqbz_row_targets_select_with_shift() {
            var title = textItem(myqbzRow, "Collected record")
            clickOn(title, 10, title.height / 2)
            var subtitle = textItem(myqbzRow, "Collected band")
            clickOn(subtitle, 10, subtitle.height / 2, Qt.ShiftModifier)
            compare(QbzMyQbz.opened.length, 0, "nothing opens in select mode")
            compare(QbzMyQbz.selectCalls.length, 2)
            compare(QbzMyQbz.selectCalls[0].position, 3)
            compare(QbzMyQbz.selectCalls[0].shift, false)
            compare(QbzMyQbz.selectCalls[1].shift, true, "Shift reaches the Rust range")
        }

        function railRow(path) {
            var list = find(rail, function (it) { return it.model !== undefined && it.itemAtIndex !== undefined })
            verify(list !== null, "the rail list exists")
            for (var i = 0; i < railView.tree.length; i++)
                if (railView.tree[i].path === path)
                    return list.itemAtIndex(i)
            return null
        }
        function railCheck(path) {
            var row = railRow(path)
            verify(row !== null, "rail row " + path + " is drawn")
            var check = find(row, function (it) {
                return it.partial !== undefined && it.on !== undefined && shown(it)
            })
            verify(check !== null, "rail checkbox " + path + " is drawn")
            return check
        }

        function test_folder_rail_shift_range_and_modifier_body_click() {
            tryVerify(function () { return railRow("/m/d") !== null }, 2000)
            clickOn(railCheck("/m/a"), 6, 6)
            clickOn(railCheck("/m/c"), 6, 6, Qt.ShiftModifier)
            compare(QbzLocal.treeCalls.length, 2)
            compare(QbzLocal.treeCalls[0], "folder:/m/a")
            var range = JSON.parse(QbzLocal.treeCalls[1].slice("range:".length))
            compare(range.map(function (n) { return n.path }).join(","), "/m/a,/m/a/1.flac,/m/b,/m/c")
            compare(range[1].isFolder, false)
            // A Ctrl click on a row body selects instead of opening the folder.
            var d = railRow("/m/d")
            clickOn(d, d.width - 20, d.height / 2, Qt.ControlModifier)
            compare(QbzLocal.treeCalls[2], "folder:/m/d")
            compare(railView.opened.length, 0)
            // A plain click on the body still opens it.
            clickOn(d, d.width - 20, d.height / 2)
            compare(railView.opened.length, 1)
            // Leaving select mode forgets the anchor: Shift is a plain toggle.
            railView.treeSelectMode = false
            railView.treeSelectMode = true
            wait(0)
            clickOn(railCheck("/m/b"), 6, 6, Qt.ShiftModifier)
            compare(QbzLocal.treeCalls[3], "folder:/m/b")
        }

        function test_settings_folder_table_shift_range() {
            var first = textItem(folderTable, "Music 1")
            clickOn(first, 10, first.height / 2)
            var fourth = textItem(folderTable, "Music 4")
            clickOn(fourth, 10, fourth.height / 2, Qt.ShiftModifier)
            compare(folderTable.selectedIds.slice().sort().join(","), "1,2,3,4")
            verify(folderTable.isSelected(2), "the numeric ids stay numbers")
        }
    }

    // ---- the selection rule itself ---------------------------------------------
    SelectionModel { id: rule }
    SelectionModel { id: keyedRule; idKey: "trackId" }
    TestCase {
        name: "SelectionRule"

        function init() {
            rule.anchorId = ""
            keyedRule.anchorId = ""
        }

        function test_numeric_ids_range() {
            var rows = [{ "id": 11 }, { "id": 12 }, { "id": 13 }, { "id": 14 }]
            var m = rule.next({}, 11, rows, Qt.NoModifier)
            m = rule.next(m, 13, rows, Qt.ShiftModifier)
            compare(Object.keys(m).sort().join(","), "11,12,13")
        }

        function test_rows_without_id_are_skipped() {
            var rows = [{ "id": "a" }, { "kind": "header" }, { "id": "b" }]
            var m = rule.next({}, "a", rows, Qt.NoModifier)
            m = rule.next(m, "b", rows, Qt.ShiftModifier)
            compare(Object.keys(m).sort().join(","), "a,b")
        }

        function test_id_key() {
            var rows = [{ "trackId": 7 }, { "trackId": 8 }, { "trackId": 9 }]
            var m = keyedRule.next({}, 9, rows, Qt.NoModifier)
            m = keyedRule.next(m, 7, rows, Qt.ShiftModifier)
            compare(Object.keys(m).sort().join(","), "7,8,9")
        }
    }
}
