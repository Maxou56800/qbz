// SelectionModel — THE selection rules, in one place.
//
// Five views (Album, Artist, Playlist, Label, LocalAlbum) each carried their
// own hand-copied `selected` map and a `toggleSelected(id)`, and none of them
// knew what a modifier key was: every click was a single toggle, so picking
// 40 tracks meant 40 clicks. Slint and Tauri have had Excel-style selection
// since 2026-06 (crates/qbz/src/selection.rs, src/lib/utils/multiSelect.ts);
// this is that port, with the rule shared instead of copied a sixth time.
//
// ── THE RULES, and where they come from ───────────────────────────────────
// Straight off Tauri's `applyShiftRange`, which is the behaviour that has
// been in the owner's hands the longest:
//
//   plain click   toggle just this row. It does NOT clear the others — in
//                 select mode the whole point is accumulating a set.
//   Ctrl / Cmd    the SAME as a plain click, and that is parity, not a
//                 shortcut: because a plain click already accumulates,
//                 "add this one without disturbing the rest" is what both
//                 already do. Tauri reads only `shiftKey` at click time and
//                 Slint matched it.
//   Shift         select the range between the anchor and this row,
//                 ADDITIVELY — a shift-click never deselects, so re-dragging
//                 a range cannot eat the set. The anchor does not move, which
//                 is what lets a range be adjusted by shift-clicking again.
//
// ── THE ANCHOR IS AN ID, NOT AN INDEX ─────────────────────────────────────
// Rows get re-sorted and filtered under a live selection — every one of these
// views has a sort control and Local Library has a filter box. An index
// anchor silently points at a different track the moment the order changes;
// an id is re-resolved against the CURRENT rows on every shift-click, and is
// simply dropped if it is no longer among them. Slint's selection.rs made the
// same call for the same reason.
//
// ── WHY IT RETURNS A MAP INSTEAD OF OWNING ONE ────────────────────────────
// Every host already owns its `selected` map and a dozen call sites read it.
// Taking that ownership away would mean rewriting all of them to find out
// whether the rule is right; handing back the NEXT map keeps each host's
// plumbing untouched and puts only the rule in here. The anchor is the one
// piece of state worth holding, because it is the one nobody had.
//
// A NEW object is always returned: mutating a `var` map in place notifies
// nothing, and every binding on it would go stale.
//
// ── EVERY CLICK TARGET MUST HAND ITS MODIFIERS OVER ───────────────────────
// The rule only sees what the click target passes in. A select-mode surface
// has more targets than its row body — the checkbox disc, the checkbox
// controls, a whole album card, the name links drawn on a row — and each one
// that called its host with no modifiers turned every Shift-click there into
// a plain toggle. `scripts/qml-tests/tst_multi_select.qml` clicks all of them
// through the real components.
//
// ── IDS COMPARE AS STRINGS ────────────────────────────────────────────────
// `anchorId` is a string property, so a numeric id (an offline track id, a
// settings folder id) was stored as "42" and then never found again among
// rows carrying 42 — Shift silently degraded to a toggle. The selection map
// keys are strings anyway, so comparing as strings is the same identity.

import QtQuick

QtObject {
    id: root

    /// The row a shift-range measures from. "" = no anchor yet. Hosts clear
    /// it when they leave select mode.
    property string anchorId: ""
    /// The row field that identifies a row. Track and album rows use `id`;
    /// the offline manager's rows carry `trackId`, the explorer facets `key`.
    property string idKey: "id"

    function _idOf(row) {
        if (!row)
            return ""
        var value = row[root.idKey]
        return value === undefined || value === null ? "" : String(value)
    }

    function _indexOf(rows, id) {
        var wanted = String(id)
        for (var i = 0; i < rows.length; i++)
            if (root._idOf(rows[i]) === wanted)
                return i
        return -1
    }

    /// Adds every identified row between the anchor and `id` to `into`.
    /// Rows without an id (group headers riding in the same list) are skipped.
    function _fillRange(into, rows, anchorAt, here) {
        var lo = Math.min(anchorAt, here)
        var hi = Math.max(anchorAt, here)
        for (var i = lo; i <= hi; i++) {
            var rowId = root._idOf(rows[i])
            if (rowId !== "")
                into[rowId] = true
        }
        return into
    }

    /// The selection map after clicking `id`, given the ordered `rows` the
    /// user is looking at and the `modifiers` off the mouse event.
    ///
    /// Pass the FILTERED rows when a view is filtered: a range over rows the
    /// user cannot see is not a range they asked for.
    function next(current, id, rows, modifiers) {
        var shift = ((modifiers || 0) & Qt.ShiftModifier) !== 0
        var anchorAt = root.anchorId !== "" ? root._indexOf(rows, root.anchorId) : -1

        if (shift && anchorAt >= 0) {
            var here = root._indexOf(rows, id)
            if (here >= 0)
                // The anchor STAYS, so the same range can be re-dragged.
                return root._fillRange(Object.assign({}, current), rows, anchorAt, here)
        }

        var key = String(id)
        var out = Object.assign({}, current)
        if (out[key] === true) delete out[key]
        else out[key] = true
        root.anchorId = key
        return out
    }

    /// The spreadsheet / file-manager rule, for lists where a plain click
    /// REPLACES the selection (the Library Explorer facets):
    ///
    ///   plain click    only this row; it becomes the anchor
    ///   Ctrl / Cmd     toggle this row, keep the rest; it becomes the anchor
    ///   Shift          only the range anchor..row; the anchor stays
    ///   Ctrl+Shift     add the range anchor..row to the selection
    ///
    /// Without a visible anchor Shift behaves like the same click without it.
    function nextExclusive(current, id, rows, modifiers) {
        var mods = modifiers || 0
        var shift = (mods & Qt.ShiftModifier) !== 0
        var additive = (mods & (Qt.ControlModifier | Qt.MetaModifier)) !== 0
        var key = String(id)
        var anchorAt = root.anchorId !== "" ? root._indexOf(rows, root.anchorId) : -1

        if (shift && anchorAt >= 0) {
            var here = root._indexOf(rows, id)
            if (here >= 0)
                return root._fillRange(additive ? Object.assign({}, current) : {},
                                       rows, anchorAt, here)
        }

        root.anchorId = key
        if (!additive) {
            var only = {}
            only[key] = true
            return only
        }
        var out = Object.assign({}, current)
        if (out[key] === true) delete out[key]
        else out[key] = true
        return out
    }
}
