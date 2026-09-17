.pragma library

// The label in `year` is localized display text. Only the numeric original
// release date participates in chronology; unknown dates remain last.
function compareDates(a, b, newest) {
    var x = Number(a.releaseSortKey || 0), y = Number(b.releaseSortKey || 0)
    if (!x || !y) return !x && !y ? 0 : (!x ? 1 : -1)
    return newest ? y - x : x - y
}

function yearOf(row) {
    return Math.floor(Number(row.releaseSortKey || 0) / 10000)
}

function groupNumber(row, group) {
    var year = yearOf(row)
    return group === "decade" ? Math.floor(year / 10) * 10 : year
}

function groupLabel(row, group) {
    var year = groupNumber(row, group)
    if (!year) return "#"
    return group === "decade" ? year + "–" + (year + 9) : String(year)
}

function textCompare(a, b) {
    var x = String(a || "").toLowerCase(), y = String(b || "").toLowerCase()
    return x < y ? -1 : (x > y ? 1 : 0)
}

function sortAlbums(rows, sort, group) {
    sort = normalizedMode(sort)
    if (group === "off") return sortRows(rows, sort)
    return rows.slice().sort(function (a, b) {
        var grouped = 0
        if (group === "year" || group === "decade") {
            var x = groupNumber(a, group), y = groupNumber(b, group)
            if (!x || !y) grouped = !x && !y ? 0 : (!x ? 1 : -1)
            else grouped = sort === "release-date-desc" ? y - x : x - y
        } else if (group === "artist") {
            grouped = textCompare(a.artist, b.artist)
        } else if (group === "alpha") {
            grouped = textCompare(String(a.title || "").charAt(0), String(b.title || "").charAt(0))
        }
        if (grouped) return grouped
        if (sort !== "default") return compareItems(a, b, sort)
        // Default retains server/feed order; alphabetical groups keep their
        // established alphabetical order within each group.
        return group === "artist" || group === "alpha" ? textCompare(a.title, b.title) : 0
    })
}

function normalizedMode(mode) {
    if (mode === "oldest") return "release-date-asc"
    if (mode === "newest") return "release-date-desc"
    return mode || "default"
}
function fieldOf(mode) {
    return normalizedMode(mode).replace(/-(asc|desc|reverse)$/, "")
}
function ascending(mode) {
    return mode === "default-reverse" || /-asc$/.test(normalizedMode(mode))
}
function compareItems(a, b, mode) {
    mode = normalizedMode(mode)
    if (mode === "default") return 0
    if (mode === "default-reverse") return Number(b._feedOrder || 0) - Number(a._feedOrder || 0)
    var field = fieldOf(mode), asc = ascending(mode)
    if (field === "release-date") return compareDates(a, b, !asc)
    if (field === "date") {
        // The REAL date when the row has one (`addedAt`: the catalog's
        // favorited_at, a purchase's purchased_at, a local favourite's own
        // stamp); a dated row always precedes an undated one; undated rows
        // fall back to the per-source recency proxy (lower rank = newer).
        // Total and transitive, so Array.sort cannot scramble it.
        var at = Number(a.addedAt || 0), bt = Number(b.addedAt || 0)
        if (at && bt) return (bt - at) * (asc ? -1 : 1)
        if (at || bt) return at ? -1 : 1
        var ar = a.added_rank === undefined ? a._feedOrder : a.added_rank
        var br = b.added_rank === undefined ? b._feedOrder : b.added_rank
        return (Number(ar || 0) - Number(br || 0)) * (asc ? -1 : 1)
    }
    var key = field === "release" ? "album" : field === "duration" ? "durationSecs"
        : field === "track-count" ? "trackCount" : field === "updated" ? "updatedAt" : field
    var x = a[key], y = b[key]
    // An empty playlist has a known zero duration even though zero-valued
    // durationSecs is omitted by the compact feed serializer.
    var emptyA = field === "duration" && a.kind === "playlist" && a.trackCount === 0
    var emptyB = field === "duration" && b.kind === "playlist" && b.trackCount === 0
    if (emptyA) x = 0
    if (emptyB) y = 0
    var numeric = field === "duration" || field === "track-count" || field === "updated"
    var missingX = x === undefined || x === null || x === "" || (field === "duration" && !x && !emptyA)
    var missingY = y === undefined || y === null || y === "" || (field === "duration" && !y && !emptyB)
    if (missingX || missingY) return missingX === missingY ? 0 : (missingX ? 1 : -1)
    return (numeric ? Number(x) - Number(y) : textCompare(x, y)) * (asc ? 1 : -1)
}
function sortRows(rows, mode) {
    if (mode === "default-reverse") return rows.slice().reverse()
    return rows.slice().sort(function (a, b) { return compareItems(a, b, mode) })
}
