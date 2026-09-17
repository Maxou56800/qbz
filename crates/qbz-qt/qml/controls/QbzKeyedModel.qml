// QbzKeyedModel — a list model that CHANGES instead of being replaced
// (2026-09-14).
//
// Handing an item view a new JS array is a model swap: QQuickItemView::
// setModel() throws every delegate away, resets the scroll offset and builds
// the page again, so one album leaving the Library grid used to rebuild the
// whole grid at the top with every cover fading back in. This model takes the
// same arrays (`rows`) but tells the view only what changed: rows whose key
// disappeared are REMOVED, new keys are INSERTED where they belong, keys that
// changed places are MOVED, and a row whose data changed is UPDATED in place.
// The view keeps its delegates, its scroll offset and its covers, and with the
// row transitions next door (QbzRowAdd / QbzRowRemove / QbzRowDisplaced) the
// leaving row fades out while its neighbours slide into the gap.
//
// WHAT THE VIEW SEES. Each model row carries two roles, `rowKey` and `rowRev`,
// never the row itself. A delegate looks its row up by key:
//
//     required property string rowKey
//     required property int rowRev
//     readonly property var modelData: keyedModel.row(rowKey, rowRev)
//     ListView.onReused: { opacity = 1; scale = 1 }   // GridView.onReused too
//
// The delegate reads the SAME JS object the host derived (no copy into a
// ListModel map, no conversion per read, and an in-place mutation of a row is
// visible to delegates built later exactly as it was with a plain array).
// `rowRev` is bumped when a row's data changed, which re-runs that binding and
// hands the delegate its new object; an unchanged row keeps its delegate
// untouched. A removed row stays readable (a small graveyard) because its
// delegate may still be fading out: Qt does not re-notify a removed item's
// roles (QQmlDMAbstractItemModelData reads its cache once the index is -1), so
// its binding keeps the value it had. The `onReused` restore matters because
// a transition cut short (a fast flick during an insert) can pool a delegate
// half faded, and the pool hands it back exactly as it was left.
//
// SCOPE. A change of `scope` (another tab, sort, search, filter set) is not a
// reconcile — the rows are replaced wholesale, WITHOUT animation (`animate`
// drops for the reset and the registered `views` lay out synchronously, so no
// transition ever sees it), which is what a tab switch has always looked like.
// Inside one scope everything reconciles and animates.
//
// COST. Keys are compared as strings in one pass; unchanged row objects are
// skipped by identity; a new object for an existing key is compared field by
// field (`ignoredKeys` excluded) and only a real difference bumps its rev. A
// reorder is applied with the fewest moves (the longest increasing run stays
// put); past `maxMoves` a reset is both cheaper and calmer than a cascade of
// slides. The reconcile is deferred with Qt.callLater, so a derive that
// changes `rows` and `scope` together lands as one operation.

import QtQuick

ListModel {
    id: root

    /// The rows to show, in order. Assign a new array whenever the source
    /// changes; the array itself does not have to be stable.
    property var rows: []
    /// A row's identity. Equal keys are the same entity; repeats are told
    /// apart by occurrence (the second "x" is "x#1").
    property var keyOf: function (row) { return row.id }
    /// Another scope replaces the rows without animation.
    property string scope: ""
    /// Row fields whose change is not a change (positional caches).
    property var ignoredKeys: []
    /// While true, `rows` changes wait and apply when it clears.
    property bool paused: false
    /// Item views bound to this model: laid out synchronously around a reset
    /// so no add/remove transition runs for it.
    property var views: []
    /// Beyond this many moves in one reconcile, reset instead.
    property int maxMoves: 48

    /// Bind each row transition's `enabled` to this.
    readonly property bool animate: root._animate
    /// The array the model currently reflects, index for index — what an
    /// index-based reader (an artwork window report) must use.
    readonly property var published: root._published

    /// After every applied change; `reset` is true for a wholesale replace.
    signal reconciled(bool reset)

    property bool _animate: true
    property var _published: []
    // Registries live on one never-reassigned object so that reading them
    // inside a delegate binding does not subscribe the binding to anything.
    property var _state: ({
        "initialized": false,
        "scope": "",
        "keys": [],
        "byKey": new Map(),
        "revs": new Map(),
        "graveyard": new Map(),
        "rev": 0
    })

    onRowsChanged: Qt.callLater(root.sync)
    onScopeChanged: Qt.callLater(root.sync)
    onPausedChanged: if (!root.paused) Qt.callLater(root.sync)
    Component.onCompleted: Qt.callLater(root.sync)

    /// The row behind `key` — also for a row that just left, while its
    /// delegate fades out. `rev` is only a binding dependency.
    function row(key, rev) {
        var st = root._state
        var r = st.byKey.get(key)
        if (r === undefined)
            r = st.graveyard.get(key)
        return r === undefined ? ({}) : r
    }

    /// Model index of `key`, or -1.
    function indexOfKey(key) {
        return root._state.keys.indexOf(key)
    }

    /// Re-hand `key`'s row to its delegate after the host mutated that row
    /// in place and a mounted delegate must re-read it.
    function touch(key) {
        var st = root._state
        var i = st.keys.indexOf(key)
        if (i < 0)
            return
        var rev = ++st.rev
        st.revs.set(key, rev)
        root.setProperty(i, "rowRev", rev)
    }

    function keysFor(rows) {
        var seen = new Map()
        var out = new Array(rows.length)
        for (var i = 0; i < rows.length; i++) {
            var k = String(root.keyOf(rows[i]))
            var n = seen.get(k) || 0
            seen.set(k, n + 1)
            out[i] = n === 0 ? k : k + "#" + n
        }
        return out
    }

    /// Field-by-field equality, `ignoredKeys` excluded. Arrays of scalars are
    /// compared element-wise; anything deeper falls back to JSON.
    function sameRow(a, b) {
        if (a === b)
            return true
        if (!a || !b || typeof a !== "object" || typeof b !== "object")
            return false
        var ignored = root.ignoredKeys
        var k
        var counted = 0
        for (k in a) {
            if (ignored.indexOf(k) >= 0)
                continue
            counted++
            if (!(k in b) || !root._sameValue(a[k], b[k]))
                return false
        }
        for (k in b) {
            if (ignored.indexOf(k) >= 0)
                continue
            counted--
        }
        return counted === 0
    }

    function _sameValue(x, y) {
        if (x === y)
            return true
        if (x === null || y === null || typeof x !== "object" || typeof y !== "object")
            return false
        if (Array.isArray(x) && Array.isArray(y) && x.length === y.length) {
            for (var i = 0; i < x.length; i++) {
                if (x[i] !== y[i]) {
                    if (typeof x[i] !== "object" || typeof y[i] !== "object")
                        return false
                    return JSON.stringify(x) === JSON.stringify(y)
                }
            }
            return true
        }
        return JSON.stringify(x) === JSON.stringify(y)
    }

    function sync() {
        if (root.paused)
            return
        var next = root.rows || []
        var keys = root.keysFor(next)
        var st = root._state
        if (!st.initialized || st.scope !== root.scope) {
            root._reset(next, keys)
            return
        }
        var old = st.keys
        var sameOrder = old.length === keys.length
        for (var s = 0; sameOrder && s < keys.length; s++)
            sameOrder = old[s] === keys[s]
        if (!sameOrder && !root._restructure(next, keys))
            return
        var changed = sameOrder ? root._update(next, keys) : true
        root._published = next
        if (changed)
            root.reconciled(false)
    }

    function _reset(next, keys) {
        var st = root._state
        var had = root.count > 0
        root._animate = false
        st.byKey = new Map()
        st.revs = new Map()
        st.graveyard = new Map()
        var batch = new Array(next.length)
        for (var i = 0; i < next.length; i++) {
            st.byKey.set(keys[i], next[i])
            batch[i] = { "rowKey": keys[i], "rowRev": 0 }
        }
        root.clear()
        if (batch.length > 0)
            root.append(batch)
        st.keys = keys
        st.scope = root.scope
        st.initialized = true
        root._published = next
        root._layoutViews(had)
        root._animate = true
        root.reconciled(true)
    }

    /// Apply the reset now, with transitions off, and — when it replaced
    /// rows the user was looking at — start from the top, as the model swap
    /// it replaces did. (A ListView lands there by itself; a GridView keeps
    /// its offset clamped into the new, shorter content.)
    function _layoutViews(toTop) {
        var vs = root.views || []
        for (var i = 0; i < vs.length; i++) {
            if (!vs[i] || typeof vs[i].forceLayout !== "function")
                continue
            vs[i].forceLayout()
            if (toTop && typeof vs[i].positionViewAtBeginning === "function")
                vs[i].positionViewAtBeginning()
        }
    }

    /// Removals, then inserts and moves; false when it fell back to a reset.
    function _restructure(next, keys) {
        var st = root._state
        var wanted = new Map()
        for (var w = 0; w < keys.length; w++)
            wanted.set(keys[w], w)

        // Survivors in their new order, and the longest run of them that is
        // already in order: those never move.
        var current = st.keys.slice()
        var positions = []
        var survivors = []
        for (var c = 0; c < current.length; c++) {
            if (wanted.has(current[c])) {
                survivors.push(current[c])
                positions.push(wanted.get(current[c]))
            }
        }
        var steady = root._longestIncreasing(positions, survivors)
        if (survivors.length - steady.size > root.maxMoves) {
            root._reset(next, keys)
            return false
        }

        // 1. Removals, bottom-up, contiguous runs in one call.
        var i = current.length - 1
        while (i >= 0) {
            if (wanted.has(current[i])) {
                i--
                continue
            }
            var end = i
            while (i - 1 >= 0 && !wanted.has(current[i - 1]))
                i--
            if (st.graveyard.size > 512)
                st.graveyard = new Map()
            for (var g = i; g <= end; g++) {
                st.graveyard.set(current[g], st.byKey.get(current[g]))
                st.byKey.delete(current[g])
                st.revs.delete(current[g])
            }
            root.remove(i, end - i + 1)
            current.splice(i, end - i + 1)
            i--
        }

        // 2. Inserts (contiguous runs) and moves, walking the new order.
        var p = 0
        var guard = 3 * keys.length + 16
        while (p < keys.length) {
            if (--guard < 0) {
                root._reset(next, keys)
                return false
            }
            if (current[p] === keys[p]) {
                p++
                continue
            }
            var key = keys[p]
            if (!st.byKey.has(key)) {
                var runEnd = p
                while (runEnd + 1 < keys.length && !st.byKey.has(keys[runEnd + 1]))
                    runEnd++
                var batch = []
                for (var r = p; r <= runEnd; r++) {
                    batch.push({ "rowKey": keys[r], "rowRev": 0 })
                    st.byKey.set(keys[r], next[r])
                }
                root.insert(p, batch)
                Array.prototype.splice.apply(current, [p, 0].concat(keys.slice(p, runEnd + 1)))
                p = runEnd + 1
                continue
            }
            if (!steady.has(key)) {
                // A row that has to move: bring it here.
                var from = current.indexOf(key, p + 1)
                if (from < 0) {
                    root._reset(next, keys)
                    return false
                }
                root.move(from, p, 1)
                current.splice(p, 0, current.splice(from, 1)[0])
                p++
                continue
            }
            // `key` stays put, so the row sitting here is one that has to
            // move further down: send it next to the first steady row that
            // follows its place in the new order.
            var floating = current[p]
            var target = wanted.get(floating)
            var anchor = -1
            for (var t = target + 1; t < keys.length; t++) {
                if (steady.has(keys[t])) {
                    anchor = current.indexOf(keys[t])
                    break
                }
            }
            var to = anchor < 0 ? current.length - 1 : anchor - 1
            if (to <= p) {
                root._reset(next, keys)
                return false
            }
            root.move(p, to, 1)
            current.splice(to, 0, current.splice(p, 1)[0])
        }

        st.keys = keys
        root._update(next, keys)
        return true
    }

    /// Hand changed rows to their delegates; true when anything changed.
    function _update(next, keys) {
        var st = root._state
        var changed = false
        for (var i = 0; i < keys.length; i++) {
            var key = keys[i]
            var prev = st.byKey.get(key)
            var row = next[i]
            if (prev === row)
                continue
            st.byKey.set(key, row)
            if (prev !== undefined && root.sameRow(prev, row))
                continue
            var rev = ++st.rev
            st.revs.set(key, rev)
            root.setProperty(i, "rowRev", rev)
            changed = true
        }
        return changed
    }

    /// Keys (of `items`) on one longest strictly increasing run of
    /// `positions` — O(n log n) patience sorting.
    function _longestIncreasing(positions, items) {
        var n = positions.length
        var tails = []
        var tailIdx = []
        var prevIdx = new Array(n)
        for (var i = 0; i < n; i++) {
            var v = positions[i]
            var lo = 0
            var hi = tails.length
            while (lo < hi) {
                var mid = (lo + hi) >> 1
                if (tails[mid] < v)
                    lo = mid + 1
                else
                    hi = mid
            }
            tails[lo] = v
            tailIdx[lo] = i
            prevIdx[i] = lo > 0 ? tailIdx[lo - 1] : -1
        }
        var out = new Set()
        var k = tails.length > 0 ? tailIdx[tails.length - 1] : -1
        while (k >= 0) {
            out.add(items[k])
            k = prevIdx[k]
        }
        return out
    }
}
