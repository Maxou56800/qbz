// Execute the production Library membership/filter functions, with no profile or API.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const read = path => fs.readFileSync(new URL('../' + path, import.meta.url), 'utf8');
const qml = read('crates/qbz-qt/qml/views/LibraryView.qml');
function method(name) {
    const start = qml.indexOf('    function ' + name + '(');
    assert(start >= 0, name);
    const end = qml.indexOf('\n    }', start);
    assert(end > start, name);
    return qml.slice(start, end + 6);
}
const sorts = vm.createContext({});
vm.runInContext(read('crates/qbz-qt/qml/assets/release-sort.js').replace('.pragma library', ''), sorts);
const ctx = vm.createContext({
    Qt: {callLater() {}}, ReleaseSort: sorts, console,
    activeTab: 'all', search: '', tabSearch: '', showLocal: true,
    showPurchases: false, showFavorites: false, showFollowing: false,
    hiresOnly: false, sortBy: 'date', sortAsc: false, activeSort: 'default',
    albumsSort: 'default', albumsGroup: 'off', tracksGroup: 'off', genreNames: [],
});
ctx.root = ctx;
vm.runInContext(['isLocalFeedItem', 'hideableLocalAlbum', 'parseFeed', 'matchesSources', 'visibleItems']
    .map(method).join('\n'), ctx);
function setFeed(rows) { ctx.feed = ctx.parseFeed(JSON.stringify(rows)); }
function visible() { return Array.from(ctx.visibleItems(), row => row.title); }
const album = (id, title, group, source, extra = {}) => ({id, title, kind: 'album',
    group, source, artist: 'Fixture', qualityTier: 'cd', ...extra});
const rows = [
    album('42', 'Favorite', 'favorites', 'qobuz'),
    album('42', 'Purchase', 'purchases', 'qobuz'),
    album('42', 'Downloaded', 'local', 'local', {sources: ['offline'], qualityTier: 'hires'}),
    album('own|album', 'Own file', 'local', 'local', {isFavorite: true, sources: ['local']}),
    album('server', 'Jellyfin', 'local', 'jellyfin'),
    album('server', 'Plex', 'local', 'plex'),
];
setFeed(rows);
assert.deepEqual(visible(), rows.map(row => row.title), 'All keeps the full feed');
ctx.activeTab = 'albums';
assert.deepEqual(visible(), ['Purchase', 'Downloaded', 'Own file', 'Jellyfin', 'Plex']);
assert.equal(ctx.feed._tabTotals.albums, 5, 'counts deduplicate catalog favorites/purchases, preserve source IDs');
ctx.showFavorites = true;
assert.deepEqual(visible(), ['Purchase', 'Own file'], 'local favorite membership survives source filtering');
ctx.showPurchases = true;
assert.deepEqual(visible(), ['Purchase'], 'favorite and purchase filters intersect');
ctx.showFavorites = false;
assert.deepEqual(visible(), ['Purchase']);
ctx.showPurchases = false;
ctx.showLocal = false;
assert.deepEqual(visible(), ['Purchase', 'Downloaded'], 'hide local keeps downloads');
ctx.showLocal = true;
ctx.hiresOnly = true;
assert.deepEqual(visible(), ['Downloaded']);
ctx.hiresOnly = false;
ctx.tabSearch = 'download';
assert.deepEqual(visible(), ['Downloaded']);
ctx.tabSearch = '';
setFeed([rows[2]]);
assert.deepEqual(visible(), ['Downloaded']);
assert.equal(ctx.feed._tabTotals.albums, 1, 'a downloads-only library must not look empty');
setFeed([rows[2], {...rows[2]}]);
assert.equal(visible().length, 1, 'duplicate source rows collapse');
assert.equal(ctx.feed._tabTotals.albums, 1);
console.log('Library regressions PASS: downloaded-only, mixed sources, ID collisions, deduplication, filters and totals');
