// Artist detail page — QML port of artist/ArtistPageView.slint.
//
// Header (200px circular portrait — right-click for the image menu
// (Add/Change/Remove custom image, View image, Open in browser, Save as…),
// left-click for the lightbox — name, bio + Read more, CircleAction
// row: Follow / Radio / Network / ⋯, From-catalog/In-library toggle),
// JUMP TO bar (jump-scroll), Popular Tracks (artwork + album column rows,
// Load more 5→all, play/shuffle-all), Latest release, release sections
// (Albums / EPs & Singles / Live / … in the official order, sort menu,
// per-section Load more paged through the core), Appears On, Playlists,
// Other (collapsed), and the 300px Network sidebar (Network/Magazine
// tabs, ORIGIN, LABELS, SIMILAR ARTISTS, RELATIONSHIPS, YOU MAY ALSO LIKE,
// and the Magazine story teasers).
//
// The document arrives in passes: the Qobuz page first, then the Magazine
// stories, then MusicBrainz Origin -> Relationships -> Discovery (see
// artist_qt.rs). Each MB section renders its own "Loading…" line and, when
// MusicBrainz is off in Settings or the artist has no confident MB match, is
// simply ABSENT — never an error frame, and nothing is requested.
//
// Blacklist banner, Artist Scene, Create Collection, both radio engines,
// multi-select and the sticky JUMP TO bar are live. The remaining artist-page
// deltas are documented beside their data producers in artist_qt.rs.
//
// "Share" left this list when the seam landed: the ⋯ entry now calls
// QbzArtist.share (artist_bridge.rs -> src/share_qt.rs), which copies
// play.qobuz.com/artist/{id} and raises the same "Link copied" toast as
// ArtistPageView.slint:530-538 / main.rs:12749.
//
// Header atmosphere (ArtistPageView.slint:120-147, 211-247): wired through
// the shared controls/HeaderGradient.qml — the SAME component AlbumView
// mounts, because the .slint paints both headers from identical blocks. It
// carries the .slint's header-colour rules with it (light text + overlay
// CircleActions while the band is on).

import "../assets/release-sort.js" as ReleaseSort
import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../cards"
import "../controls"
import "../rows"
import "../shell"
import "../theme"

Rectangle {
    id: root
    // Transparent while the ambient background is active (phase 14 —
    // HomeView.slint:163: the frosted content panel shows through).
    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn

    // Round to the AppShell content-frame bezel (Radius.md): QML clips
    // are rectangular, so the frame's own rounding never reaches the
    // view — the view's own fill must round instead.
    radius: 12

    QbzTheme { id: theme }

    readonly property var artist: JSON.parse(QbzArtist.artistJson)

    // ---- In-page search (JumpNavBar's magnifier) -------------------------
    // artist.rs `filter_artist` (main.rs:14991 wires ArtistActions.search to
    // it) is a PURE client-side filter over Popular Tracks, Appears On and
    // every release-section album — no backend round-trip — so it ports as a
    // filter over the parsed document rather than a bridge call.
    property string searchQuery: ""
    readonly property string needle: searchQuery.trim().toLowerCase()

    function matchTrack(t) {
        return needle === ""
            || (t.title || "").toLowerCase().indexOf(needle) >= 0
            || (t.artist || "").toLowerCase().indexOf(needle) >= 0
    }

    readonly property var topTracks: {
        var all = artist.topTracks || []
        if (root.needle === "") return all
        var out = []
        for (var i = 0; i < all.length; i++) if (matchTrack(all[i])) out.push(all[i])
        return out
    }
    readonly property var appearsOn: {
        var all = artist.appearsOn || []
        if (root.needle === "") return all
        var out = []
        for (var i = 0; i < all.length; i++) if (matchTrack(all[i])) out.push(all[i])
        return out
    }
    // A section whose albums all filter out DISAPPEARS (artist.rs:1033), and
    // Load more is suppressed while a filter is active (:1042 — appending
    // would bring back unfiltered items).
    //
    // ---- THE PAGED TAIL IS MERGED HERE, NOT PUSHED INTO `artist` ----------
    // Owner, 2026-08-02: "el error esta en que nunca cargan mas albumes."
    // The old handler parsed the page and pushed it into
    // `artist.releaseSections[i].cards` IN PLACE, then fired artistChanged().
    // Both Repeaters (`model: releaseSections` at :1997/:2114 and
    // `model: section.cards` inside ReleaseSection) were then handed the very
    // SAME array object they already held, and the unfiltered arm below used
    // to `return all` — the identical array again. Nothing was dirty, so
    // nothing rebuilt: the page was fetched, parsed and appended to a data
    // structure no view ever re-read. This tree already writes the rule down,
    // in cards/PlaylistCollage.qml: "Rebind requires a NEW object reference."
    // (PRE-EXISTING, not a regression from today's smooth-append work — that
    // pass only added the `releaseMoreLanded` call to the handler. What it
    // changed is that the button now shows an in-flight placeholder, which is
    // what made a silent no-op visible.)
    //
    // So the appended pages live in `root.releaseOverlay` (see its header)
    // and are merged HERE, which builds a NEW section object with a NEW cards
    // array whenever the overlay has anything for that bucket — the one thing
    // that actually invalidates both Repeaters. `artist` is never mutated, so
    // a republish of the document (the progressive Magazine / MusicBrainz
    // passes) cannot lose the tail either.
    //
    // De-dup by album id is MANDATORY on this path and not merely defensive:
    // artist_qt.rs `merge_release_page` (:686) folds every landed page into
    // the STASHED document too, so the moment the next enrichment pass
    // republishes, the document already carries the cards the overlay holds.
    // Without the id filter every appended album would appear twice from that
    // frame on. (Qobuz can also repeat a row across pages by itself.)
    readonly property var releaseSections: {
        var all = artist.releaseSections || []
        var out = []
        for (var i = 0; i < all.length; i++) {
            var src = all[i]
            var extra = root.releaseOverlay[src.releaseType]
            var cards = src.cards || []
            // The bucket's persisted sort, stamped on the row by
            // artist_qt.rs `map_artist` / `resort_section`. It has to be COPIED
            // into every rebuilt literal below: an object literal carries only
            // the keys it names, so a `sortBy` left out here makes the header's
            // QbzSelect snap back to "Default" the moment this arm is taken.
            var sortBy = src.sortBy || "default"
            // `hasMore` is only overridden once a page has actually LANDED
            // (`extra.hasMore` stays undefined otherwise): the document's copy
            // is the truth until then, and blanking it would hide the button —
            // and with it the in-flight placeholder QbzLoadMore draws under it.
            var hasMore = src.hasMore === true
            var k
            if (extra !== undefined) {
                var seen = {}
                var merged = []
                for (k = 0; k < cards.length; k++) {
                    merged.push(cards[k])
                    seen[cards[k].id] = true
                }
                var tail = extra.cards || []
                for (k = 0; k < tail.length; k++) {
                    if (seen[tail[k].id]) continue
                    seen[tail[k].id] = true
                    merged.push(tail[k])
                }
                // THE TAIL IS APPENDED, SO IT MUST BE RE-SORTED HERE.
                // artist.rs:1245-1258 `append_release_page` re-sorts after
                // folding a page in, "before computing the artwork jobs at
                // their post-sort positions" — under "A–Z" a page-2 album
                // belongs wherever its title puts it, not at the bottom.
                // artist_qt.rs `merge_release_page` does that to the STASHED
                // document; this does it to the copy actually on screen, which
                // is a DIFFERENT array (the overlay never reaches the stash).
                // Fixing only one of the two leaves the grid correct until the
                // next enrichment republish, or wrong until it — see the
                // overlay's own header for why the two copies exist.
                cards = root.sortReleaseCards(merged, sortBy)
                if (extra.hasMore !== undefined) hasMore = extra.hasMore === true
            }
            if (root.needle === "") {
                // No overlay for this bucket -> hand back the document's own
                // object: nothing was appended, so there is nothing to rebind
                // and a fresh wrapper would only churn delegates. (The document
                // arrives already sorted — artist_qt.rs sorts at page build and
                // on every `setSectionSort` — so there is nothing to do to it.)
                out.push(extra === undefined ? src
                         : { "releaseType": src.releaseType, "title": src.title,
                             "cards": cards, "hasMore": hasMore, "sortBy": sortBy, "sortLoading": src.sortLoading === true })
                continue
            }
            // Filtered arm: the merge happened FIRST, so an appended album
            // that matches the needle is searchable like any other — but
            // `hasMore` is still forced false (artist.rs:1042) and only the
            // KEPT cards travel, so the overlay can never leak a filtered-out
            // card into this arm.
            var kept = []
            for (var j = 0; j < cards.length; j++)
                if ((cards[j].title || "").toLowerCase().indexOf(root.needle) >= 0)
                    kept.push(cards[j])
            if (kept.length === 0) continue
            out.push({ "releaseType": src.releaseType, "title": src.title,
                       "cards": kept, "hasMore": false, "sortBy": sortBy })
        }
        return out
    }
    /// JS twin of `artist_qt::sort_release_cards`, itself the port of
    /// `album_map::sort_album_items` (crates/qbz/src/album_map.rs:244-264).
    ///
    /// Both exist because the bucket exists TWICE: Rust owns the stashed
    /// document (page 1 plus every page `merge_release_page` folded in) and
    /// sorts it there, while `releaseOverlay` holds the pages Load more brought
    /// in on THIS side and is concatenated onto the document's array by the
    /// projection above — an array Rust never sees. The two must agree, so the
    /// arms are kept identical.
    ///
    /// Returns a NEW array (`slice()` before `sort()`): the projection binds to
    /// the result, and sorting the caller's array in place would hand the
    /// Repeaters the same reference back — "Rebind requires a NEW object
    /// reference" (cards/PlaylistCollage.qml).
    ///
    /// Sort by the original numeric date; `year` is a localized label.
    /// Default preserves the server order, as in the official client.
    function sortReleaseCards(cards, sort) {
        function byTitle(a, b) {
            var x = (a.title || "").toLowerCase(), y = (b.title || "").toLowerCase()
            return x < y ? -1 : (x > y ? 1 : 0)
        }
        switch (sort) {
        case "relevant":
        case "default": return cards.slice().sort(function (a, b) { return (a.defaultOrder || 0) - (b.defaultOrder || 0) })
        case "oldest": return cards.slice().sort(function (a, b) { return ReleaseSort.compareDates(a, b, false) })
        case "newest": return cards.slice().sort(function (a, b) { return ReleaseSort.compareDates(a, b, true) })
        case "title-asc": return cards.slice().sort(byTitle)
        case "title-desc": return cards.slice().sort(function (a, b) { return byTitle(b, a) })
        default: return cards
        }
    }
    readonly property var labels: artist.labels || []
    readonly property var similarArtists: artist.similarArtists || []
    readonly property var playlists: artist.playlists || []
    // MusicBrainz-driven sidebar payload (artist_qt.rs ArtistNetwork). Absent
    // on the very first frame of a cold document — every read below is
    // defaulted so a missing member can never throw.
    readonly property var network: artist.network || ({})
    readonly property var mbOrigin: network.origin || ({})
    readonly property var mbRelationships: network.relationships || ({})
    readonly property var stories: artist.stories || []

    // ---- Artist Scene: the gate BOTH doors share (contract §2.1) ---------
    //
    // Three terms, and dropping any one of them breaks a door:
    //
    //  1. `locationClickable` — computed in Rust (artist_qt.rs `map_origin`)
    //     so QML never re-derives the reference's unparenthesised guard. A
    //     country-only location has nothing to drill into.
    //  2. `mbid !== ""` — and NOT `network.mbAvailable`, which is the trap.
    //     That flag is the user's MusicBrainz OPT-IN and nothing else
    //     (artist_qt.rs:982-983, whose doc comment used to claim otherwise);
    //     it is seeded SYNCHRONOUSLY with the first artist frame while `mbid`
    //     stays "" until the async publish_network lands. Gating on it alone
    //     lights both doors up on frame one with no id to hand them, and the
    //     click fires with an empty parameter.
    //  3. `mbAvailable` — the MusicBrainz opt-in, which is now trustworthy at
    //     THIS point in time: the settings toggle republishes the open artist
    //     document (settings_qt.rs, "musicbrainz" arm -> republish_open_artist),
    //     so a stale `locationClickable: true` can no longer survive on a page
    //     the user navigates back to after switching the integration off.
    readonly property bool sceneAvailable:
        mbOrigin.locationClickable === true
        && (network.mbid || "") !== ""
        && network.mbAvailable === true

    /// Both doors call THIS — never `QbzScene.open` directly — so they can
    /// never drift apart in what they pass.
    function openArtistScene() {
        if (!sceneAvailable)
            return
        QbzScene.open(network.mbid || "",
                      mbOrigin.artistName || artist.name || "",
                      mbOrigin.locationAreaId || "",
                      mbOrigin.locationCity || "",
                      mbOrigin.locationCountry || "",
                      mbOrigin.locationCountryCode || "",
                      mbOrigin.locationPrecision || "",
                      (mbOrigin.seedGenres || []).join(","),
                      (mbOrigin.seedTags || []).join(","))
    }

    property var coverMap: ({})
    property string activeJumpTab: "popular-tracks"
    property string artistTab: "catalog"

    // ---- "In library" tab (#737 round, contract §3/§4) ---------------------
    // The rows ride INSIDE the artist document (`artist.libraryItems`,
    // artist_qt.rs) — favourites ∪ purchases of this artist, one row per
    // entity, the purchase row representative and `isFavorite` meaning "also
    // hearted". The two switches below are the `Library > Albums / Tracks`
    // source switches with THEIR semantics (LibraryView.qml:566-590: neither
    // = union, one = that source, both = intersection) but LOCAL state: the
    // Library view's own switches must not reach into this page.
    property bool libShowPurchases: false
    property bool libShowFavorites: false
    /// Preview 10 (Popular Tracks previews 5), "Load more" opens the whole
    /// list at once — no paging, the rows are already in hand.
    readonly property int libPreview: 10
    property bool libTracksExpanded: false
    readonly property var libItems: {
        var src = artist.libraryItems || []
        var out = []
        for (var i = 0; i < src.length; i++) {
            var it = src[i]
            var purchased = it.group === "purchases"
            var hearted = it.isFavorite === true
            var included
            if (root.libShowPurchases && root.libShowFavorites) included = purchased && hearted
            else if (root.libShowPurchases) included = purchased
            else if (root.libShowFavorites) included = hearted
            else included = true
            if (included) out.push(it)
        }
        return out
    }
    readonly property var libTracks: libItems.filter(function (x) { return x.kind === "track" })
    readonly property var libAlbums: libItems.filter(function (x) { return x.kind === "album" })
    /// The queue a row click / "Play all" builds from: the WHOLE filtered
    /// list in render order, not only the revealed preview — a click on row
    /// 3 must be followed by rows 4..n exactly like Popular Tracks queues its
    /// full list. Handed to `QbzLibrary.libraryPlayVisible` / `libraryPlayAll`
    /// (library_qt::play_from_visible), the source-aware seam LibraryView
    /// uses — never `QbzPlayer.playArtistTrack`, whose queue is the POPULAR
    /// list and which starts it at row 0 for any id it does not hold (#737).
    function libTrackIds() {
        var ids = []
        for (var i = 0; i < root.libTracks.length; i++) ids.push(String(root.libTracks[i].id))
        return ids
    }
    /// The library Albums section's sort — the release buckets' five keys,
    /// seeded from the document (`artist.librarySort`, artist_prefs "library")
    /// and persisted through the same invokable the buckets use. Sorted here,
    /// not in Rust: the rows already live in QML (`sortReleaseCards` reads
    /// `title` / `year`, which a FeedItem carries).
    property string libSort: "default"
    readonly property var libAlbumsSorted: root.sortReleaseCards(root.libAlbums, root.libSort)
    function setLibSort(key) {
        root.libSort = key
        QbzArtist.setSectionSort("library", key)
    }

    // ---- Album-section play (the split button) -------------------------------
    // Every album section — each catalog bucket and the library Albums grid —
    // carries a QbzSplitButton right of its sort: the body plays the whole
    // section in its SORT order as one queue (`QbzPlayer.playAlbums`), the
    // chevron offers Play random (ONE album of the section, replacing the
    // queue) and Play selected, which arms a per-section select mode: the
    // cards grow a check bottom-right inside the art, the button reads
    // "Play selected (n)" and plays the checked albums in sort order, and the
    // menu swaps that row for Cancel selection. One section is armed at a
    // time (`albumSelectKey` is the bucket's releaseType or "library").
    // No queue options (next / later / queue) on purpose.
    property string albumSelectKey: ""
    property var albumSelected: ({})
    readonly property int albumSelectedCount: Object.keys(root.albumSelected).length
    function albumSelectToggle(id) {
        var m = Object.assign({}, root.albumSelected)
        if (m[id]) delete m[id]
        else m[id] = true
        root.albumSelected = m
    }
    function albumSelectClear() {
        root.albumSelectKey = ""
        root.albumSelected = ({})
    }
    function albumSectionLabel(key) {
        return root.albumSelectKey === key
            ? QbzSession.tr("Play selected", QbzSession.trRev) + " (" + root.albumSelectedCount + ")"
            : QbzSession.tr("Play all", QbzSession.trRev)
    }
    function albumSectionMenu(key) {
        var m = [
            { "label": QbzSession.tr("Play all", QbzSession.trRev), "icon": "play-fill", "action": "play-all" },
            { "label": QbzSession.tr("Play random", QbzSession.trRev), "icon": "shuffle", "action": "play-random" }
        ]
        if (root.albumSelectKey === key)
            m.push({ "label": QbzSession.tr("Cancel selection", QbzSession.trRev), "icon": "x", "action": "cancel-selection" })
        else
            m.push({ "label": QbzSession.tr("Play selected", QbzSession.trRev), "icon": "square-check-big", "action": "play-selected" })
        return m
    }
    function albumSectionIds(cards) {
        var ids = []
        for (var i = 0; i < cards.length; i++) ids.push(String(cards[i].id))
        return ids
    }
    /// `cards` is the section's list in its CURRENT sort order.
    function albumSectionAction(key, cards, action) {
        var ids = root.albumSectionIds(cards)
        if (action === "play-all") {
            root.albumSelectClear()
            if (ids.length > 0) QbzPlayer.playAlbums(JSON.stringify(ids))
        } else if (action === "play-random") {
            root.albumSelectClear()
            if (ids.length > 0) QbzPlayer.playAlbum(ids[Math.floor(Math.random() * ids.length)])
        } else if (action === "play-selected") {
            root.albumSelectKey = key
            root.albumSelected = ({})
        } else if (action === "cancel-selection") {
            root.albumSelectClear()
        }
    }
    /// The button body: Play all, or — armed — the checked albums in sort
    /// order, then disarm.
    function albumSectionPrimary(key, cards) {
        if (root.albumSelectKey !== key) {
            root.albumSectionAction(key, cards, "play-all")
            return
        }
        var ids = []
        for (var i = 0; i < cards.length; i++)
            if (root.albumSelected[cards[i].id] === true) ids.push(String(cards[i].id))
        root.albumSelectClear()
        if (ids.length > 0) QbzPlayer.playAlbums(JSON.stringify(ids))
    }

    // ---- Header atmosphere (ArtistPageView.slint:120-147) ----------------
    // Same three-line rule as AlbumView (the .slint says "same rule as
    // AlbumPageView" at :120). The pref is read LIVE off the settings
    // snapshot where one exists, else off the document (artist_qt.rs).
    readonly property bool headerGradientPref: {
        var raw = QbzBridge.settingsJson
        if (raw && raw.length > 2) {
            try {
                var d = JSON.parse(raw)
                if (d.albumHeaderGradient !== undefined)
                    return d.albumHeaderGradient === true
            } catch (e) { /* fall through to the document copy */ }
        }
        return artist.headerGradient !== false
    }
    readonly property bool headerAtmoOn: headerGradientPref && !ambientOn
    readonly property bool headerLight: headerGradientPref || ambientOn
    readonly property color hdrStrong: headerLight ? "#ffffff" : theme.textPrimary
    readonly property color hdrBody: headerLight ? "#e0ffffff" : theme.textSecondary
    readonly property bool hdrOverlay: headerLight
    /// Slint's `Theme.text-primary` as an ICON tint, for the hovers that raise
    /// a muted glyph on a THEME surface (every consumer sits on
    /// `surface-elevated`/transparent, never on the artwork header — that one
    /// uses `hdrStrong` above). Runtime-tinted via src/icon_tint_qt.rs, so it
    /// is the live token; it used to be `isDark ? "primary" : "black"`, a
    /// two-value stand-in from when only fixed bakes existed.
    readonly property string tintOnSurface: "textPrimary"
    /// TrackRow.slint:123-125 — the row hover uses the polarity-baked alpha
    /// ramp "so the hover state is visible on light themes too (the old
    /// #ffffff16 was invisible white-on-white there)". The zebra stripe is
    /// deliberately left as the literal, per the same comment.
    readonly property color rowHoverBg: theme.alphaTiers.length > 0
        ? theme.alphaTier(8) : (theme.isDark ? "#14ffffff" : "#14000000")
    property bool topTracksExpanded: false
    property bool appearsOnExpanded: false
    property bool otherExpanded: false

    // ---- Release-section "Load more": in-flight + append-fade state -------
    // controls/QbzLoadMore.qml owns the button and the placeholder row under
    // it; the APPEARANCE of what lands is the HOST's job (that file's "WHAT
    // THIS CONTROL DOES *NOT* DO" note) because only this view knows the index
    // the appended page starts at.
    //
    // Both maps live on `root`, keyed by releaseType, and NOT on the
    // ReleaseSection instances: the Repeater at :1787 re-creates every
    // delegate whenever `artist` changes (a `var` property notifies on every
    // write, and the document is republished once per enrichment pass —
    // stories, then each MusicBrainz section), and one of those republishes is
    // the very frame the new page lands in. A flag parked on the section would
    // be destroyed exactly when it was needed.
    //
    // `releasePending` is COPY-ON-WRITE because the ReleaseSection BINDS
    // `busy` to it, so it has to notify. `releaseFade` is mutated IN PLACE on
    // purpose: it is read once, imperatively, by each card's
    // Component.onCompleted, and a notify there would re-run bindings for
    // nothing.
    property var releasePending: ({})
    /// releaseType -> { from: <index the appended page starts at>,
    ///                  at: <ms the page LANDED, 0 while in flight> }.
    property var releaseFade: ({})

    /// ---- The appended pages themselves -----------------------------------
    /// releaseType -> { cards: [<pages 2..n, deduped, NEVER page 1>],
    ///                  hasMore: <the newest page's has_more; undefined until
    ///                            a page has landed>,
    ///                  pages: <index pages loaded so far, 1 = the embedded
    ///                          bucket — the LOADED_PAGES twin, see
    ///                          releaseMoreLanded's cap note> }.
    ///
    /// The document owns page 1 and stays the source of truth for it; this
    /// overlay owns everything Load more brought in, and `releaseSections`
    /// (:115) merges the two into a NEW array so the Repeaters rebuild. It is
    /// COPY-ON-WRITE for exactly that reason — the projection binds to it, so
    /// a write has to notify.
    ///
    /// Why an overlay and not a push into `artist`: `artist` is
    /// `JSON.parse(QbzArtist.artistJson)` (:52), a BINDING. Every progressive
    /// republish (Magazine stories, then each MusicBrainz section — see
    /// artist_qt.rs) re-runs that parse and yields a brand-new object, so
    /// anything written onto the parsed copy is discarded on the next pass.
    /// This is the same reasoning `localToggles` (:437) already carries for
    /// the heart/pin state.
    property var releaseOverlay: ({})
    /// releaseType -> { sent: <offset the page in flight asked for>,
    ///                  next: <offset the NEXT page must ask for> }.
    ///
    /// The paging cursor is the count of rows RECEIVED from the server, which
    /// is NOT the count on screen: both sides drop duplicate ids (here and in
    /// artist_qt.rs `merge_release_page`), so after a page that repeated three
    /// rows the on-screen count trails the server cursor by three. Handing the
    /// on-screen count back as the offset would re-request rows already held,
    /// and the dedup would then swallow them — a Load more that visibly does
    /// nothing, which is the very bug this block exists to kill.
    ///
    /// Mutated IN PLACE on purpose, same as `releaseFade`: it is written from
    /// the landing handler and read imperatively from the button's onClicked,
    /// and no binding reads it. A notifying write here would tear down and
    /// rebuild every AlbumCard in every release grid on each click, for
    /// nothing.
    property var releaseCursor: ({})
    /// How long after a landing the threshold stays live. The republish that
    /// carries the page re-creates the delegates in the same tick, so this is
    /// generous; what it buys is that a LATER, unrelated republish (a
    /// MusicBrainz pass) re-creates that tail WITHOUT re-fading it.
    readonly property int releaseFadeWindowMs: 1500

    /// Called from the section's Load more BEFORE the bridge call, so the
    /// threshold is the count that was on screen at click time. RETURNS the
    /// offset the caller must pass to the bridge (see `releaseCursor`).
    ///
    /// `sorted` = this bucket carries a non-default sort. The append fade is
    /// then SUPPRESSED, deliberately: `releaseFadeFrom` hands back an INDEX and
    /// the card delegate fades everything at or past it, which only means "the
    /// page that just landed" while new rows sit at the TAIL. Under "A–Z" the
    /// projection interleaves them (see `sortReleaseCards`), so that band would
    /// dissolve an arbitrary run of cards the user has been looking at all
    /// along. The reference has no fade at all here, so no fade is strictly
    /// closer to it than the wrong one — and tracking the new ids through a
    /// re-sort is not a thing the .slint does either.
    function releaseMoreRequested(releaseType, loadedCount, sorted) {
        if (sorted === true) delete releaseFade[releaseType]
        else releaseFade[releaseType] = { "from": loadedCount, "at": 0 }
        var c = releaseCursor[releaseType]
        // First page of this bucket: nothing has been fetched through the
        // paging endpoint yet, so the document's own row count IS the server
        // cursor. Afterwards the cursor is authoritative.
        var off = (c !== undefined && c.next !== undefined) ? c.next : loadedCount
        releaseCursor[releaseType] = { "sent": off,
                                       "next": (c !== undefined ? c.next : undefined) }
        var m = Object.assign({}, root.releasePending)
        m[releaseType] = true
        root.releasePending = m
        // artist_bridge.rs emits releaseSectionReady only on SUCCESS
        // (main.rs:872 just logs the error and returns), so a failed page would
        // otherwise leave the skeleton up forever. Bound it with the same 8s
        // QbzLoadMore's own skeleton settles at, after which the button comes
        // back armed.
        releaseSettle.restart()
        return off
    }
    /// The page landed (or came back empty): fold it into the overlay, advance
    /// the cursor, arm the fade window and disarm the placeholder.
    ///
    /// `cards` is the RAW page as the bridge sent it; `hasMore` is the
    /// server's own flag for the bucket.
    ///
    /// ORDER IS LOAD-BEARING. The overlay write is COPY-ON-WRITE, so it
    /// notifies, so `releaseSections` re-evaluates and the Repeater at :1997
    /// destroys and rebuilds every section delegate — QbzLoadMore and every
    /// AlbumCard included — SYNCHRONOUSLY, inside that assignment. Everything
    /// those rebuilt delegates read has to be true BEFORE it happens:
    ///   - `releaseFade[rt].at`, or each appended card's Component.onCompleted
    ///     asks `releaseFadeFrom()` while the landing still reads "in flight"
    ///     (at === 0 -> MAX_VALUE) and the page snaps in with no fade at all;
    ///   - `releasePending`, or the button is re-created with `busy` still
    ///     true and the skeleton flashes back for a frame UNDER the page that
    ///     just landed (the note the old handler carried, kept verbatim in
    ///     spirit).
    /// So: cursor + fade + placeholder first, overlay LAST.
    function releaseMoreLanded(releaseType, cards, hasMore) {
        // ---- 1. the cursor (in place — nothing binds to it) ---------------
        // Advance by the RAW page length, not by what survived the dedup: the
        // server counted every row it sent.
        var c = releaseCursor[releaseType]
        var base = (c !== undefined && c.sent !== undefined) ? c.sent : 0
        releaseCursor[releaseType] = { "sent": base, "next": base + cards.length }

        // ---- 2. fade window + placeholder ---------------------------------
        if (releaseFade[releaseType] !== undefined)
            releaseFade[releaseType].at = Date.now()
        if (root.releasePending[releaseType] !== undefined) {
            var m = Object.assign({}, root.releasePending)
            delete m[releaseType]
            root.releasePending = m
        }

        // ---- 3. the overlay (COPY-ON-WRITE — the projection binds to it) ---
        var prev = root.releaseOverlay[releaseType]
        var tail = (prev !== undefined && prev.cards !== undefined)
                   ? prev.cards.slice() : []
        var seen = {}
        var i
        // Page 1 lives in the document, and once artist_qt.rs
        // `merge_release_page` has folded an earlier page into the STASHED
        // document a republish puts that page there too — so both are checked
        // before anything is kept, and the overlay never grows a row the
        // document already carries.
        var doc = root.artist.releaseSections || []
        for (i = 0; i < doc.length; i++) {
            if (doc[i].releaseType !== releaseType) continue
            var dc = doc[i].cards || []
            for (var j = 0; j < dc.length; j++) seen[dc[j].id] = true
            break
        }
        for (i = 0; i < tail.length; i++) seen[tail[i].id] = true
        var before = tail.length
        for (i = 0; i < cards.length; i++) {
            if (seen[cards[i].id]) continue
            seen[cards[i].id] = true
            tail.push(cards[i])
        }
        // The reference CAPS the index at 4 pages per bucket and hands off to
        // the discography page beyond that: artist.rs:718-720 ("Page 1 is the
        // embedded bucket; 3 more loads reach the cap" — MAX_INDEX_PAGES = 4),
        // enforced at append_release_page:1268. This port has that page now
        // (QbzArtist.openReleases, the section title's own click), so the cap
        // carries over 1:1 — an uncapped index button would page a 300-album
        // Singles bucket into this grid forever, which is exactly what the
        // dedicated page exists to absorb.
        var pages = ((prev !== undefined && prev.pages !== undefined) ? prev.pages : 1) + 1
        // …and `!appended_ids.is_empty()` is the same line's other clause: a
        // page that contributed NOTHING after the dedup (all rows already on
        // screen, or empty outright) kills the button no matter what the flag
        // says — leaving it up would offer a page that can never change the
        // grid, and every further click would burn a request and land here
        // again.
        var more = (hasMore === true) && (tail.length > before) && (pages < 4)
        var o = Object.assign({}, root.releaseOverlay)
        o[releaseType] = { "cards": tail, "hasMore": more, "pages": pages }
        root.releaseOverlay = o
    }
    /// Index the appended page of `releaseType` starts at — +inf when this
    /// section never paged, when its page has not landed yet, or when the
    /// landing is already stale. Fading unconditionally would re-dissolve the
    /// whole grid on every enrichment pass, which reads as a flicker, not as
    /// polish.
    function releaseFadeFrom(releaseType) {
        var e = releaseFade[releaseType]
        if (e === undefined || e.at === 0)
            return Number.MAX_VALUE
        if (Date.now() - e.at > root.releaseFadeWindowMs)
            return Number.MAX_VALUE
        return e.from
    }
    /// The user picked a sort for one bucket (ReleaseGrid.slint:81-89). Hand it
    /// to the bridge — `QbzArtist.setSectionSort` persists it by release_type
    /// and re-sorts the STASHED document, which comes back as a republished
    /// `artistJson` — and reconcile the local paging state first.
    ///
    /// The OVERLAY's CARDS for this bucket are dropped. Nothing is lost by
    /// that: an overlay entry is only ever written when a page LANDS, and by
    /// then artist_qt.rs `load_release_page` has already folded that page into
    /// the stashed document (`merge_release_page`, called before the signal is
    /// emitted) — so every card the overlay holds is in the document the
    /// republish is about to carry, now in the new order. Keeping the cards
    /// would leave a second, stale copy of those rows whose only remaining job
    /// is to be deduped away.
    ///
    /// `hasMore` and `pages` are KEPT (the entry survives as a card-less
    /// stub). They are paging state, not row data, and neither is in the
    /// document: merge_release_page writes the SERVER's has_more flag into the
    /// stash, not the capped one, so dropping the whole entry after the cap
    /// closed a bucket would let the document's `hasMore: true` resurrect the
    /// button — and the reference's LOADED_PAGES counter survives a sort
    /// change too (nothing in artist.rs `resort_section` touches it).
    ///
    /// The CURSOR is deliberately kept. It counts rows the SERVER has sent, and
    /// the server's own order (`release_date`, hardcoded on purpose — the five
    /// picker keys are applied locally and never reach the API) has not changed
    /// just because we re-ordered them here. Resetting it would make the next
    /// Load more re-request pages we already hold, which the dedup would then
    /// swallow — the "button does nothing" bug the cursor exists to prevent.
    ///
    /// The FADE threshold is dropped for the same reason `releaseMoreRequested`
    /// suppresses it under a custom sort: it is an index into an order that no
    /// longer exists, and the republish re-creates every delegate.
    function releaseSortChanged(releaseType, sortKey) {
        delete releaseFade[releaseType]
        var sections = root.artist.releaseSections || []
        var oldSort = "default"
        for (var i = 0; i < sections.length; i++) {
            if (sections[i].releaseType === releaseType) oldSort = sections[i].sortBy || "default"
        }
        var serverChanged = (oldSort === "relevant") !== (sortKey === "relevant")
        var overlay = Object.assign({}, root.releaseOverlay)
        if (serverChanged) {
            delete overlay[releaseType]
            delete root.releaseCursor[releaseType]
            var pending = Object.assign({}, root.releasePending)
            delete pending[releaseType]
            root.releasePending = pending
        } else if (overlay[releaseType] !== undefined) {
            var e = Object.assign({}, overlay[releaseType])
            delete e.cards
            overlay[releaseType] = e
        }
        root.releaseOverlay = overlay
        QbzArtist.setSectionSort(releaseType, sortKey)
    }
    Timer {
        id: releaseSettle
        interval: 8000
        repeat: false
        onTriggered: root.releasePending = ({})
    }

    // ---- Network sidebar open/closed -------------------------------------
    // ShellState.content-constrained (state.slint:4114): window under the NPB
    // breakpoint AND a right panel (Queue / Lyrics) open. NOT a raw
    // `root.width < N` — the .slint calls that exact trigger out as the
    // regression it fixed (ArtistPageView.slint:166-172): at a normal window
    // the content area is already narrow WITHOUT any panel, so the sidebar
    // would never auto-open at all.
    readonly property bool contentConstrained:
        Window.width > 0 && Window.width < 1366
        && (QbzShell.queueOpen || QbzShell.lyricsOpen)
    // A constrain edge closes the full-width relationship sidebar so the
    // release grid keeps priority. This is deliberately ONE-WAY: after that
    // the existing Network button remains available and may reopen it even in
    // the constrained layout; gaining room must not override that user choice.
    // A newly opened artist still starts from the available-space default in
    // syncArtistState(), matching the page-entry behaviour.
    property bool networkOpen: !contentConstrained
    onContentConstrainedChanged: {
        if (contentConstrained)
            networkOpen = false
    }
    property string netTab: "network"
    readonly property int preview: 5
    // Sidebar lists are unbounded upstream (an orchestra can list 150
    // members): show a slice, expand on demand — the delegates for the rest
    // are never instantiated.
    readonly property int sidebarPreview: 12
    property bool membersExpanded: false
    property bool groupsExpanded: false
    property bool collabsExpanded: false
    // Thumbs-downed discovery rows, by mbid. Session-only: the Slint app
    // persists these in its `discovery_dismiss` store, which this POC does
    // not open, so the rejection lasts as long as the process — it is NOT
    // written anywhere and makes no claim to be.
    property var dismissedDiscovery: ({})
    // The artist the view state (tab choice, dismissals) belongs to. Compared
    // on every republish so a mid-load pass never resets the user's choices.
    property string loadedArtistId: ""

    // Optimistic heart/pin state. The document is republished several times
    // per page now (stories, then each MusicBrainz section), and every
    // republish re-parses `artist` — a toggle written straight onto the parsed
    // object would silently pop back. Overrides live here instead and win over
    // whatever the document says, until the artist changes.
    property var localToggles: ({})
    function toggleState(key, fallback) {
        return localToggles[key] !== undefined ? localToggles[key] : fallback === true
    }
    function setToggleState(key, value) {
        var m = localToggles
        m[key] = value
        localToggles = Object.assign({}, m)
    }

    // --- Artist blacklist -------------------------------------------------
    // ONE source for the two surfaces the .slint drives off the same
    // ArtistState.is-blacklisted: the overflow-menu row label
    // (ArtistPageView.slint:565-567) and the hidden-artist banner (:595, :600).
    // `artist_qt` seeds the field on entry; the local toggle map provides the
    // optimistic edge while `blacklistChanged` settles or rolls it back.
    readonly property bool artistBlacklisted: toggleState("artistBlacklist", artist.isBlacklisted)
    function toggleBlacklist() {
        var aid = artist.id || ""
        if (aid === "")
            return
        // Optimistic flip first (main.rs:12777 `st.set_is_blacklisted(!was)`),
        // then the mutation; `blacklistChanged` settles it — or rolls it back
        // on a failed write (blacklist_qt.rs `artist_toggle`).
        setToggleState("artistBlacklist", !artistBlacklisted)
        QbzBlacklist.artistToggle(aid, artist.name || "")
    }

    readonly property var discoveryRows: {
        var out = []
        var rows = network.discovery || []
        for (var i = 0; i < rows.length; i++) {
            if (!dismissedDiscovery[rows[i].mbid]) out.push(rows[i])
        }
        return out
    }

    // ---- Loading staging (artist_qt.rs publishes in passes) --------------
    // The Qobuz page lands first; the Magazine stories and each MusicBrainz
    // sidebar section arrive later on their own flags. Every one of these is
    // ALSO gated on mbAvailable upstream, so with MusicBrainz off in Settings
    // (or no confident match) they are absent — placeholder included.
    readonly property bool primaryLoading: QbzArtist.artistLoading
                                           && (artist.topTracks || []).length === 0
    readonly property bool originPending: network.mbAvailable === true
                                          && network.originLoading === true
    readonly property bool relationshipsPending: network.mbAvailable === true
                                                 && network.relationshipsLoading === true
    readonly property bool discoveryPending: network.mbAvailable === true
                                             && network.discoveryLoading === true
                                             && root.discoveryRows.length === 0
    readonly property bool similarPending: similarArtists.length === 0 && QbzArtist.artistLoading
    readonly property bool storiesPending: artist.storiesLoading === true && stories.length === 0

    // ONE 900ms phase for every placeholder on the page (QbzSkeleton's COST
    // note: N placeholders, 1 timer). Stops dead when nothing is pending.
    Timer {
        id: skeletonPhase
        property bool on: false
        interval: 900
        repeat: true
        running: root.visible && (root.primaryLoading || root.originPending
                                  || root.relationshipsPending || root.discoveryPending
                                  || root.similarPending || root.storiesPending)
        onTriggered: on = !on
    }

    // JUMP TO tabs from the present sections (ArtistState.jump-tabs).
    // Built from the RAW document, never the filtered lists: artist.rs
    // builds jump_tabs ONCE at load (`build_jump_tabs`) and `filter_artist`
    // does not touch them, so the strip must not reshuffle per keystroke.
    readonly property var jumpTabs: {
        var tabs = []
        // The strip is a SCROLL navigator over one vertical page, so it can
        // only list what that page currently draws. In library mode that is
        // the Tracks and Albums sections that have rows — the catalog set
        // stayed up before and "Popular Tracks" scrolled to a section that
        // was not there (owner capture, 2026-09-04).
        if (root.artistTab === "library") {
            // "About" is the strip's back-to-top, and in library mode it is
            // also what keeps the strip MOUNTED when the filter yields nothing
            // (purchases ∧ favourites on an artist whose purchases are not
            // hearted): the bar carries the two switches, so an empty tab set
            // would have hidden the only way to reset them.
            tabs.push({ "id": "about", "label": QbzSession.tr("About", QbzSession.trRev) })
            if (root.libTracks.length > 0) tabs.push({ "id": "tracks", "label": QbzSession.tr("Tracks", QbzSession.trRev) })
            if (root.libAlbums.length > 0) tabs.push({ "id": "albums", "label": QbzSession.tr("Albums", QbzSession.trRev) })
            return tabs
        }
        var rawTop = artist.topTracks || []
        var rawSections = artist.releaseSections || []
        var rawAppears = artist.appearsOn || []
        if ((artist.bio || "") !== "") tabs.push({ "id": "about", "label": QbzSession.tr("About", QbzSession.trRev) })
        if (rawTop.length > 0) tabs.push({ "id": "popular-tracks", "label": QbzSession.tr("Popular Tracks", QbzSession.trRev) })
        for (var i = 0; i < rawSections.length; i++) {
            if (rawSections[i].releaseType !== "other")
                tabs.push({ "id": rawSections[i].releaseType, "label": rawSections[i].title })
        }
        if (rawAppears.length > 0) tabs.push({ "id": "appears-on", "label": QbzSession.tr("Appears On", QbzSession.trRev) })
        return tabs
    }

    // Two blocks, not one: artwork is QbzLibrary's signal and the releases
    // pager is QbzArtist's. Retargeting a mixed block wholesale would
    // silently orphan the other half — QML resolves handlers lazily, so the
    // discography would just stop loading with nothing in the log.
    // Covers arrive ONE AT A TIME, and on a warm cache `sidebar_artwork_window`
    // emits the whole disk-hit set in a single synchronous loop (main.rs:391).
    // Rebinding `coverMap` per arrival is quadratic in the page: each arrival
    // copied the entire map and re-evaluated the cover binding of EVERY
    // mounted card, so an artist with 200 releases did ~40k binding
    // evaluations and 200 map copies during the frames the page is trying to
    // paint. Arrivals are coalesced into ONE rebind per frame — the same fix
    // LocalLibraryView carries, at O(n) instead of O(n²), with the covers
    // still appearing progressively (16ms granularity is invisible).
    // --- Track info for the Popular Tracks rows (2026-09-13) ---------------
    // ONE lazy modal per artist view (the full Popup tree must not exist per
    // row). Activated on demand, torn down on close (Qt.callLater so the
    // Popup is not destroyed mid-signal) — the same shape rows/TrackRow.qml
    // carries per row.
    Loader {
        id: artistTrackInfo
        active: false
        sourceComponent: TrackInfoModal { }
    }
    Connections {
        target: artistTrackInfo.item
        ignoreUnknownSignals: true
        function onClosed() { Qt.callLater(function () { artistTrackInfo.active = false }) }
    }
    function openTrackInfo(trackId) {
        if (!trackId || trackId === "")
            return
        artistTrackInfo.active = true
        artistTrackInfo.item.openFor(trackId)
    }

    property var _coverInbox: ({})
    Timer {
        id: coverFlush
        interval: 16
        repeat: false
        onTriggered: {
            var m = Object.assign({}, root.coverMap, root._coverInbox)
            root._coverInbox = ({})
            // A rebind needs a NEW object reference (same-ref assignment is
            // not a change in QML).
            root.coverMap = m
        }
    }
    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            root._coverInbox[key] = path
            if (!coverFlush.running) coverFlush.start()
        }
        // The SETTLED follow/heart state from Rust: the flipped value when the
        // write landed, the UNCHANGED one when it failed. Both the header
        // follow and the Popular Tracks / Appears On hearts write their
        // optimistic flip into localToggles and nothing used to correct it, so
        // a failed write stayed visibly wrong until the user left the page.
        // Key shape is `library_qt::feed_key` (`{kind}:{id}`) — which is
        // already the exact key the track rows use for their override.
        function onLibraryFavoriteChanged(key, value) {
            var aid = (artist && artist.id) ? artist.id : ""
            if (aid !== "" && key === "artist:" + aid)
                root.setToggleState("artist", value)
            else if (key.indexOf("track:") === 0 || key.indexOf("album:") === 0)
                root.setToggleState(key, value)
        }
        // Header pin, same seam: the overflow-menu row flips `artistPin`
        // optimistically, and this settles it from the store (Slint does the
        // same for the open detail view: `st.set_pinned(pinned)` when its id
        // matches). The release CARDS need nothing here — each one listens to
        // this signal itself (cards/AlbumCard.qml).
        function onPinChanged(key, value) {
            var aid = (artist && artist.id) ? artist.id : ""
            if (aid !== "" && key === "artist:" + aid)
                root.setToggleState("artistPin", value)
        }
    }
    // Blacklist settle / rollback. `blacklistChanged` carries the state the
    // write actually produced — flipped on success, UNCHANGED on failure
    // (blacklist_qt.rs `artist_toggle`), which is exactly what main.rs:12799
    // does with its rollback branch. Also the cross-surface walk: unblocking
    // the same artist from the manager view while this page is mounted moves
    // the menu label and drops the banner. Same two-arg `{kind}:{id}` shape as
    // the two signals above; its own Connections block only because the signal
    // lives on a different singleton.
    Connections {
        target: QbzBlacklist
        function onBlacklistChanged(key, value) {
            var aid = (artist && artist.id) ? artist.id : ""
            if (aid !== "" && key === "artist:" + aid)
                root.setToggleState("artistBlacklist", value)
        }
    }

    Connections {
        target: QbzArtist
        function onReleaseSectionReady(releaseType, cardsJson, hasMore) {
            // Hand the page to the OVERLAY (root.releaseOverlay) and let the
            // `releaseSections` projection merge it. It used to be pushed
            // straight into `root.artist.releaseSections[i].cards` followed by
            // a manual `artistChanged()`, and that is why "nunca cargan mas
            // albumes": the push mutated the array IN PLACE, so both Repeaters
            // were re-handed the identical array reference and QML had nothing
            // to invalidate — the fake `artistChanged()` re-ran every OTHER
            // binding on the page and left the two that mattered untouched,
            // because `releaseSections`' unfiltered arm returned `all`, the
            // document's own array, unchanged.
            //
            // The signal is emitted only on SUCCESS (main.rs:872 logs errors
            // and returns), and an empty page comes through here too — that is
            // what clears the skeleton when a bucket runs out.
            var cards = JSON.parse(cardsJson)
            root.releaseMoreLanded(releaseType, cards, hasMore)
            // The appended covers are not in the document (artist_qt.rs folds
            // them into the STASHED copy, which only reaches us on the next
            // enrichment republish), so the document-driven dispatch would
            // leave the new row as blank tiles until an unrelated pass
            // happened to land. dispatchCovers() reads the overlay too, and
            // `dispatchedCovers` makes the re-scan free.
            root.dispatchCovers()
        }
    }
    Component.onCompleted: {
        syncArtistState()
        dispatchCovers()
    }
    onArtistChanged: {
        syncArtistState()
        invalidatePortraitOverride()
        dispatchCovers()
        // The last owned row just left (a republish after an un-heart): the
        // toggle unmounts, so the page must not stay on an empty tab with no
        // way back.
        if (artistTab === "library" && (artist.libraryCount || 0) === 0)
            artistTab = "catalog"
    }
    // Cover dispatch keys off the raw document (artist.artUrl etc.), so
    // re-fire when the parsed value actually changes (same stale race).
    // (the raw document drives the dispatch; onArtistChanged above covers it)
    onArtistTabChanged: {
        // A selection is a list of THIS mode's rows; it cannot survive the
        // switch. The strip re-anchors on the first section the new mode
        // draws (catalog keeps its Popular Tracks default).
        setMultiSelect(false)
        albumSelectClear()
        // The first SECTION, not "About" (back-to-top) when a section exists.
        var first = ""
        for (var i = 0; i < root.jumpTabs.length; i++) {
            first = root.jumpTabs[i].id
            if (first !== "about") break
        }
        root.activeJumpTab = artistTab === "catalog" ? "popular-tracks" : first
        if (artistTab === "library") {
            // Re-arm the warm, served from cache (main.rs warm_library).
            QbzArtist.warmLibrary()
            dispatchLibCovers()
        }
    }

    // The document is republished several times per page (stories, then each
    // MusicBrainz section). Reset per-artist view state ONLY when the id
    // actually changed, or an enrichment pass would yank the sidebar tab back
    // under the user mid-read.
    // --- Multi-select (Popular Tracks + Appears On) ------------------------
    // One selection model for the page's Qobuz track lists. Same shape as
    // AlbumView/PlaylistView: state in QML, select-all/clear local, the rest
    // via QbzPlayer.bulkTracksAction (bulk_tracks_qt.rs).
    property bool multiSelect: false
    property var selected: ({})
    readonly property int selectedCount: Object.keys(root.selected).length
    readonly property bool multiSelectOn: root.multiSelect
    function setMultiSelect(on) {
        root.multiSelect = on
        if (!on) { root.selected = ({}); sel.anchorId = "" }
    }
    /// Excel-style selection lives in ONE place — controls/SelectionModel.qml
    /// holds the anchor and the Shift-range rule; this view keeps owning its
    /// map. The two lists are CONCATENATED in the order they are drawn, so a
    /// range can run from the last top track into "Appears on" exactly as it
    /// looks on screen — `selectedIdsInOrder` already walks them that way.
    SelectionModel { id: sel }
    /// The track lists a selection can span, in draw order, for the ACTIVE
    /// mode: the catalog's Popular Tracks + Appears On, or the library tab's
    /// Tracks. `onArtistTabChanged` clears the selection on a switch, so a
    /// range never mixes the two.
    function selectableLists() {
        return root.artistTab === "library" ? [root.libTracks]
                                            : [root.topTracks, root.appearsOn]
    }
    function toggleSelected(id, mods) {
        var lists = selectableLists()
        var all = lists.length > 1 ? lists[0].concat(lists[1]) : lists[0]
        root.selected = sel.next(root.selected, id, all,
                                 mods === undefined ? Qt.NoModifier : mods)
    }
    function selectedIdsInOrder() {
        var out = []
        var lists = selectableLists()
        for (var l = 0; l < lists.length; l++) {
            var rows = lists[l]
            for (var i = 0; i < rows.length; i++)
                if (root.selected[rows[i].id] === true) out.push(rows[i].id)
        }
        return out
    }
    function bulkAction(action) {
        if (action === "select-all") {
            var m = {}
            var lists = selectableLists()
            for (var l = 0; l < lists.length; l++)
                for (var i = 0; i < lists[l].length; i++) m[lists[l][i].id] = true
            root.selected = m
            return
        }
        if (action === "clear") { root.selected = ({}); sel.anchorId = ""; return }
        var ids = root.selectedIdsInOrder()
        if (ids.length === 0) return
        QbzPlayer.bulkTracksAction(JSON.stringify(ids), action, "artist", String(artist.id || ""))
        if (action !== "add-to-playlist" && action !== "add-to-mixtape")
            root.selected = ({})
    }
    // Ctrl+A / Escape hotkey router seam (AppShell duck-types these).
    function selectAll() {
        if (!root.multiSelect) root.setMultiSelect(true)
        root.bulkAction("select-all")
    }
    function exitMultiSelectMode() {
        if (root.multiSelect) root.setMultiSelect(false)
    }

    function syncArtistState() {
        var id = artist.id || ""
        if (id === loadedArtistId)
            return
        loadedArtistId = id
        setMultiSelect(false)
        // A fresh artist opens on its catalog with the library tab's
        // preview and switches at rest.
        artistTab = "catalog"
        libTracksExpanded = false
        libShowPurchases = false
        libShowFavorites = false
        libSort = artist.librarySort || "default"
        albumSelectClear()
        // Slint opens a fresh artist on Network, or on Magazine when
        // MusicBrainz is off (an empty Network tab is worse than none).
        netTab = (artist.network && artist.network.mbAvailable) ? "network" : "magazine"
        // …and re-applies the room rule (ArtistPageView.slint:178, artist.rs
        // `reset_network_sidebar`): a new artist re-opens the panel unless
        // the content area is constrained.
        networkOpen = !contentConstrained
        dismissedDiscovery = ({})
        localToggles = ({})
        // A fresh artist must never inherit the previous one's paging state:
        // a stale threshold would fade a band of the new artist's first page
        // for no reason, a stale pending flag would show a skeleton under a
        // button nobody pressed, a stale OVERLAY would graft the previous
        // artist's albums onto this one's grids (release types are shared
        // keys — every artist has an "album" bucket), and a stale CURSOR would
        // page the new artist from the old one's offset.
        releasePending = ({})
        releaseFade = ({})
        releaseOverlay = ({})
        releaseCursor = ({})
        releaseSettle.stop()
        membersExpanded = false
        groupsExpanded = false
        collabsExpanded = false
        dispatchedCovers = ({})
    }

    function dispatchLibCovers() {
        var items = root.libItems || []
        var urls = []
        for (var i = 0; i < items.length; i++) if (items[i].imageUrl) urls.push(items[i].imageUrl)
        dispatchArtwork(urls)
    }

    // Already-requested artwork keys. With the progressive republish the
    // dispatch runs once per pass, so re-sending the whole (potentially
    // several-hundred-entry) URL list every time is pure waste — send only
    // what is new.
    property var dispatchedCovers: ({})
    function dispatchArtwork(urls) {
        var fresh = []
        for (var i = 0; i < urls.length; i++) {
            var u = urls[i]
            if (!u || dispatchedCovers[u]) continue
            dispatchedCovers[u] = true
            fresh.push(u)
        }
        if (fresh.length > 0) QbzShell.sidebarArtworkWindow(JSON.stringify(fresh))
    }

    // A custom portrait is consulted by `artwork_qt::cached_path` (it checks
    // `cover_artwork_qt::override_for_url` FIRST), so once one exists
    // `coverMap[artist.artUrl]` holds the OVERRIDE file, not the Qobuz one.
    // Removing the override has to invalidate that entry as well, or the
    // header and the lightbox keep showing the picture the user just deleted:
    // `dispatchedCovers` dedupes by url and is cleared only on an artist
    // change, so the republish alone never re-resolves it. Add/Change need the
    // same invalidation in reverse.
    property string _lastCustomPortrait: ""
    function invalidatePortraitOverride() {
        var cur = (artist && artist.customImagePath) ? artist.customImagePath : ""
        if (cur === root._lastCustomPortrait) return
        root._lastCustomPortrait = cur
        var url = (artist && artist.artUrl) ? artist.artUrl : ""
        if (url === "") return
        var m = Object.assign({}, root.coverMap)
        delete m[url]
        root.coverMap = m
        // Drop the dedupe key too, so the dispatchCovers() that follows in the
        // same handler actually re-requests it.
        delete root.dispatchedCovers[url]
    }

    // THE RULE FOR THIS FUNCTION: every section of the page that binds
    // `root.coverMap` has to contribute its urls here, or its covers are
    // never requested and the section renders empty tiles forever — nothing
    // downstream reports the omission, because a missing key is
    // indistinguishable from a cover that has not landed yet.
    //
    // Three sections were missing and each one rendered blank:
    //   - Latest release (`artist.lastRelease`, the reported bug) — one card,
    //     and the only cover between Popular Tracks and the release grids;
    //   - Appears On (`artist.appearsOn`) — TrackRow covers, the same
    //     PopularTrackRow component the collected topTracks use, which is why
    //     the omission was easy to miss;
    //   - Playlists (`artist.playlists`) — the 200px rectangle covers of the
    //     horizontal strip.
    // Present and collected: the header portrait, Popular Tracks, every
    // release section (including the collapsed "Other"), and the Magazine
    // stories. The Network sidebar carries NO covers (ArtistSimilar and
    // MbDiscoveryJson have no artUrl — artist_qt.rs), so it contributes none.
    // The In-library tab has its own dispatcher, dispatchLibCovers().
    function dispatchCovers() {
        var urls = []
        if (artist.artUrl) urls.push(artist.artUrl)
        var i, j
        // RAW lists: a filtered-out card still needs its cover for when the
        // query is cleared, and this must not re-run per keystroke.
        var rawTop = artist.topTracks || []
        var rawAppears = artist.appearsOn || []
        var rawSections = artist.releaseSections || []
        var rawPlaylists = artist.playlists || []
        for (i = 0; i < rawTop.length; i++) if (rawTop[i].artUrl) urls.push(rawTop[i].artUrl)
        for (i = 0; i < rawAppears.length; i++)
            if (rawAppears[i].artUrl) urls.push(rawAppears[i].artUrl)
        if (artist.lastRelease && artist.lastRelease.artUrl)
            urls.push(artist.lastRelease.artUrl)
        for (i = 0; i < rawSections.length; i++)
            for (j = 0; j < (rawSections[i].cards || []).length; j++)
                if (rawSections[i].cards[j].artUrl) urls.push(rawSections[i].cards[j].artUrl)
        // …and the pages "Load more" appended, which are NOT in the document:
        // they live in root.releaseOverlay until artist_qt.rs's stashed copy
        // reaches us through the next enrichment republish. Off the overlay
        // rather than off `releaseSections` for the same reason the lists
        // above are RAW — the projection is filtered, and a covers dispatch
        // must not depend on the search box.
        for (var rt in root.releaseOverlay) {
            var tail = root.releaseOverlay[rt].cards || []
            for (i = 0; i < tail.length; i++)
                if (tail[i].artUrl) urls.push(tail[i].artUrl)
        }
        for (i = 0; i < rawPlaylists.length; i++)
            if (rawPlaylists[i].artUrl) urls.push(rawPlaylists[i].artUrl)
        // Magazine story thumbnails ride the same pipeline (arc-cdn URLs).
        // Off `artist.stories`, NOT the derived `stories` property (:110): this
        // function is called from `onArtistChanged`, and a derived binding still
        // holds the PREVIOUS document inside its source's change handler on this
        // Qt build (measured — the same race QueuePanel.dispatchCovers and
        // AlbumView's `onHeaderChanged` exist for). The stories land in a LATER
        // publish than the page itself, so a stale read asked for the previous
        // pass's thumbnails and the story cards stayed empty frames whenever the
        // stories pass was the last publish.
        var rawStories = artist.stories || []
        for (i = 0; i < rawStories.length; i++)
            if (rawStories[i].artUrl) urls.push(rawStories[i].artUrl)
        dispatchArtwork(urls)
    }

    function scrollToSection(id) {
        root.activeJumpTab = id
        // Anchors resolve against the column of the ACTIVE mode: the catalog
        // and library columns are siblings in the page, and a library tab
        // resolved against the hidden catalog column would scroll to that
        // column's coordinates.
        var host = root.artistTab === "library" ? libraryTab : sectionAnchors
        for (var i = 0; i < host.children.length; i++) {
            var c = host.children[i]
            if (c.anchorId === id) {
                flick.contentY = host.y + c.y - 48
                return
            }
        }
        // "About" is back-to-top: the library column carries no such anchor
        // (the bio lives in the header), so it lands on the page top.
        if (id === "about") flick.contentY = 0
    }



    // Popular Tracks row (TrackRow with artwork + album column).
    //
    // COLUMN GEOMETRY: rows/TrackCols.qml, the same object rows/TrackRow.qml
    // and rows/TrackListHeader.qml read. This component is a FORK of the
    // shared row (POC-NOTE, pre-existing) and it had the full column set
    // re-typed as literals; they were numerically right, but a fork with its
    // own copy of the numbers is precisely how a table's columns drift. The
    // artist page draws no column header (neither does
    // artist/ArtistPageView.slint), so nothing is misaligned today — this
    // keeps it that way if the widths ever move.
    component PopularTrackRow: Rectangle {
        id: popRow
        property var row: ({})
        property int rowIndex: 0
        property bool showAlbum: true
        /// Where a play on this row goes — `function (id)`. Unset, the row
        /// plays through `QbzPlayer.playArtistTrack`, whose queue is the
        /// POPULAR list (artist_qt TOP_QUEUE). The "In library" tab passes
        /// its own handler because a library row is NOT in that queue and
        /// `top_queue` starts the popular list at row 0 for any id it does
        /// not hold — issue #737. The row never asks which tab it is in.
        property var playHandler: null
        function play() {
            if (popRow.playHandler) popRow.playHandler(String(popRow.row.id))
            else QbzPlayer.playArtistTrack(popRow.row.id)
        }
        // Multi-select (the view's bulk bar): in select mode the whole row
        // body is the toggle and the leading cell swaps to a round checkbox
        // (TrackRow.slint:173-177, 392-417).
        property bool selectMode: false
        property bool checked: false
        /// `modifiers` off the mouse event — see SelectionModel.
        signal toggleSelect(int modifiers)
        // Live offline status for the row (seeded in the document; updates
        // arrive on QbzShell's trackCacheStatusChanged).
        property int cacheStatus: row.cacheStatus !== undefined ? row.cacheStatus : 0
        Connections {
            target: QbzShell
            function onTrackCacheStatusChanged(trackId, status, progress) {
                if (trackId === (popRow.row.id || "")) popRow.cacheStatus = status
            }
        }

        TrackCols { id: cols }

        readonly property bool isActive: QbzPlayer.npTrackId !== "" && QbzPlayer.npTrackId === row.id
        readonly property bool hovered: trArea.containsMouse || favArea.containsMouse || moreArea.containsMouse

        // --- PULLED FROM THE QOBUZ CATALOGUE (contract §5.2) --------------
        // The artist page is one of the seven surfaces §5.2 names, and this
        // component is a documented FORK of rows/TrackRow.qml (see the header
        // above), so the treatment has to be repeated here rather than
        // inherited. It is repeated EXACTLY — same field, same predicate, same
        // `circle-alert` in the number slot, same 0.5 dim — because two
        // screens that disagree about what a dead row looks like is the
        // fork-drift this file has already paid for once.
        //
        // `row.qobuzUnavailable` is the API's `streamable: false` for THIS
        // track and nothing else: an absent field is not a claim, so a
        // producer that does not carry it yet leaves every row alive.
        // `cacheStatus === 3` (downloaded) is excluded because a pulled track
        // the user already downloaded still plays from disk (§5.3, F5) — this
        // fork has no cached-but-pulled badge of its own, so such a row simply
        // keeps behaving normally, which is the safe half of that split.
        readonly property bool pulled: row.qobuzUnavailable === true
        readonly property bool pulledDead: popRow.pulled && popRow.cacheStatus !== 3

        width: parent ? parent.width : 0
        height: 50
        radius: 8
        // Hover fill off on a dead row — a row that lights up reads clickable.
        color: (hovered && !popRow.pulledDead)
            ? root.rowHoverBg : (rowIndex % 2 === 1 ? "#07ffffff" : "transparent")

        Rectangle {
            visible: isActive
            x: 2
            y: 7
            width: 3
            height: parent.height - 14
            radius: 1.5
            color: theme.accent
        }

        Row {
            anchors.fill: parent
            anchors.leftMargin: cols.padH
            anchors.rightMargin: cols.padH
            spacing: cols.gap
            // THE DIM. On the Row, not on `popRow`: the now-playing edge mark
            // and the row menu are siblings of this item and neither fades.
            // 0.5 is views/purchases/PurchaseListRow.qml's value, the same one
            // rows/TrackRow.qml adopted.
            opacity: popRow.pulledDead ? 0.5 : 1.0

            // Position number (artwork rows carry it separate from the cover).
            // On a dead row the alert glyph takes this slot — same cell, same
            // width, so nothing reflows. `circle-alert`, NOT `triangle-alert`:
            // the download column below already draws the triangle for a
            // FAILED CACHE (`cacheStatus === 4`), and one row must not carry
            // two identical glyphs meaning two different things (§A F11).
            Item {
                visible: showAlbum
                width: cols.colNumber
                height: parent.height
                Text {
                    visible: !popRow.pulledDead
                    anchors.fill: parent
                    verticalAlignment: Text.AlignVCenter
                    text: row.number
                    color: theme.textMuted
                    font.pixelSize: 13
                    horizontalAlignment: Text.AlignHCenter
                    elide: Text.ElideRight
                }
                Loader {
                    anchors.centerIn: parent
                    width: 20
                    height: 20
                    // A Loader, so a live row instantiates neither the glyph
                    // nor the attached ToolTip (one QObject per row the moment
                    // it is referenced).
                    active: popRow.pulledDead
                    visible: active
                    sourceComponent: Item {
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "circle-alert"
                            width: 15
                            height: 15
                            tintName: "favorite"
                        }
                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            ToolTip.visible: containsMouse
                            ToolTip.delay: 300
                            // Same split as rows/TrackRow.qml: a pre-release
                            // says "not yet", a pulled track keeps the REUSED
                            // msgid the reactive skip path toasts.
                            ToolTip.text: row.qobuzUpcoming === true
                                ? QbzSession.tr("This track is not yet available", QbzSession.trRev)
                                : QbzSession.tr("This track is no longer available", QbzSession.trRev)
                        }
                    }
                }
            }
            // Cover with hover play overlay — in select mode this cell is
            // the row's round checkbox instead.
            //
            // RADIUS 4, not `theme.radiusSm` (8): rows/TrackRow.qml:553 — the
            // SHARED row this component is a fork of, and the one PlaylistView
            // / the album detail / search all draw — rounds its artwork cell
            // at 4. The fork had drifted to 8 and the owner saw it ("los album
            // arts de Popular tracks esta mas redondo que otros, por ejemplo,
            // los de playlistview"). All THREE stacked elements carry the same
            // number — the cell, the image and the hover scrim — or the scrim's
            // corners stick out past the art. The ROW's own `radius: 8` above
            // is correct and matches TrackRow.qml:288; only the cover moved.
            Rectangle {
                width: showAlbum ? cols.colArt : cols.colNumber
                height: showAlbum ? cols.colArt : 28
                anchors.verticalCenter: parent.verticalCenter
                radius: 4
                color: theme.surfaceElevated
                clip: true
                // The select-mode checkbox (accent fill when checked).
                Rectangle {
                    visible: popRow.selectMode
                    anchors.centerIn: parent
                    width: 14
                    height: 14
                    radius: 7
                    color: popRow.checked ? theme.accent : "transparent"
                    border.width: popRow.checked ? 0 : 1.5
                    border.color: theme.textMuted
                    QbzIcon {
                        visible: popRow.checked
                        anchors.centerIn: parent
                        name: "check"
                        width: 10
                        height: 10
                        tintName: theme.accentGlyphTint
                    }
                }
                QbzIcon {
                    visible: !popRow.selectMode
                    anchors.centerIn: parent
                    name: "music"
                    width: showAlbum ? 16 : 14
                    height: showAlbum ? 16 : 14
                    tintName: "muted"
                }
                RoundedImage {
                    visible: !popRow.selectMode
                    anchors.fill: parent
                    source: root.coverMap[row.artUrl] || ""
                    radius: 4
                }
                // The hover scrim and its play glyph are the cover cell's play
                // AFFORDANCE, and an affordance is a claim that clicking does
                // something — so a dead row shows neither, and the area under
                // them is disabled rather than left to issue a play that can
                // only fail.
                Rectangle {
                    visible: !popRow.selectMode && !popRow.pulledDead
                    anchors.fill: parent
                    radius: 4
                    color: "#000000"
                    opacity: trArea.containsMouse || isActive ? 0.6 : 0.0
                    Behavior on opacity { NumberAnimation { duration: 150 } }
                }
                QbzIcon {
                    visible: !popRow.selectMode && !popRow.pulledDead
                        && (trArea.containsMouse || isActive)
                    anchors.centerIn: parent
                    name: isActive && QbzPlayer.npPlaying ? "pause" : "play-fill"
                    width: 16
                    height: 16
                    // On the #000000 @ 0.6 artwork scrim above — dark under
                    // every theme.
                    tintName: "white"
                }
                MouseArea {
                    anchors.fill: parent
                    enabled: !popRow.selectMode && !popRow.pulledDead
                    cursorShape: Qt.PointingHandCursor
                    onClicked: popRow.play()
                }
            }
            // Title + artist.
            //
            // This was `- 6 * 14` for the gaps in BOTH arms. With the album
            // column on there are nine visible cells, i.e. EIGHT gaps, so the
            // stretch column ran 28px long and dragged Album / Duration /
            // Quality / heart 28px right of where the shared TrackRow puts
            // them — the same class of defect as the header, inside a forked
            // row. `cols.titleWidth` counts the gaps from the arms.
            //
            // Arms: with the album column the leading cells are the 32px
            // number AND the 36px cover (artwork arm); without it the single
            // 32px cover IS the number cell.
            Column {
                width: cols.titleWidth(popRow.width, popRow.showAlbum, popRow.showAlbum,
                                       true, true, true)
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Row {
                    spacing: 6
                    Text {
                        text: row.title
                        // Muted, not merely dimmed: the Row's 0.5 fades the
                        // cell uniformly, and the TITLE is the part that has to
                        // stop reading as live content.
                        color: popRow.pulledDead ? theme.textMuted : theme.textPrimary
                        font.pixelSize: 14
                        font.weight: theme.weightMedium
                        elide: Text.ElideRight
                        width: Math.min(implicitWidth, parent.parent.width - (row.explicit ? 22 : 0))
                    }
                    Rectangle {
                        visible: row.explicit
                        width: 16
                        height: 16
                        radius: 3
                        anchors.verticalCenter: parent.verticalCenter
                        color: theme.surfaceElevated
                        Text {
                            anchors.centerIn: parent
                            text: "E"
                            color: theme.textMuted
                            font.pixelSize: 9
                            font.weight: theme.weightSemibold
                        }
                    }
                }
                Text {
                    width: parent.width
                    visible: row.artist !== ""
                    text: row.artist
                    color: theme.textMuted
                    font.pixelSize: 13
                    elide: Text.ElideRight
                }
            }
            // Album column.
            Text {
                id: albumCell
                visible: showAlbum
                width: showAlbum ? cols.colAlbum : 0
                anchors.verticalCenter: parent.verticalCenter
                text: row.album
                color: row.albumId !== "" && albumLinkArea.containsMouse ? theme.textPrimary : theme.textMuted
                font.pixelSize: 13
                elide: Text.ElideRight
                MouseArea {
                    id: albumLinkArea
                    anchors.fill: parent
                    enabled: row.albumId !== ""
                    hoverEnabled: true
                    cursorShape: row.albumId !== "" ? Qt.PointingHandCursor : Qt.ArrowCursor
                    onClicked: QbzAlbum.openAlbum(row.albumId)
                }
            }
            Text {
                width: cols.colDuration
                anchors.verticalCenter: parent.verticalCenter
                text: row.duration
                color: theme.textMuted
                font.pixelSize: 13
                horizontalAlignment: Text.AlignHCenter
            }
            // The SAME bare badge the shared TrackRow's quality cell mounts
            // (rows/TrackRow.qml:1048): tier label stacked over the exact
            // "24-bit / 96 kHz" line. This fork used to render a lone tier
            // word ("HI-RES") even though the document has carried
            // qualityDetail all along.
            Item {
                width: cols.colQuality
                height: parent.height
                QualityBadgeFull {
                    anchors.centerIn: parent
                    tier: row.qualityTier || ""
                    detail: row.qualityDetail || ""
                    showIcon: false
                    bare: true
                    maxTextWidth: cols.colQuality
                }
            }
            // Favorite (live). Reads through the override map so the state
            // survives a document republish (see root.localToggles).
            Rectangle {
                property bool favorite: root.toggleState("track:" + row.id, row.isFavorite)
                width: cols.colFavorite
                height: cols.colFavorite
                radius: theme.radiusSm
                anchors.verticalCenter: parent.verticalCenter
                color: favArea.containsMouse ? theme.surfaceElevated : "transparent"
                QbzIcon {
                    // The SLOT stays and only the glyph goes: this fork lays
                    // out from the same TrackCols object the shared row and its
                    // header use, so collapsing the cell would shift every
                    // column left of it.
                    visible: !popRow.pulledDead
                    anchors.centerIn: parent
                    name: parent.favorite ? "heart-filled" : "heart"
                    width: 16
                    height: 16
                    tintName: parent.favorite
                        ? "favorite"
                        : (favArea.containsMouse ? root.tintOnSurface : "muted")
                }
                MouseArea {
                    id: favArea
                    anchors.fill: parent
                    enabled: !popRow.selectMode && !popRow.pulledDead
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        root.setToggleState("track:" + row.id, !parent.favorite)
                        QbzLibrary.libraryToggleFavorite("track", row.id)
                    }
                }
            }
            // Offline download — live since the offline port (the fork used
            // to carry an inert stub). Same status vocabulary as the shared
            // TrackRow: 0 none · 1 queued · 2 downloading · 3 ready · 4 failed.
            Rectangle {
                width: cols.colDownload
                height: cols.colDownload
                anchors.verticalCenter: parent.verticalCenter
                color: "transparent"
                QbzIcon {
                    // A pulled track has no file url to fetch, so the download
                    // affordance could only fail — the slot keeps its width,
                    // the glyph goes.
                    visible: popRow.cacheStatus === 0 && !popRow.pulledDead
                    anchors.centerIn: parent
                    name: "cloud-download"
                    width: 16
                    height: 16
                    tintName: popDlArea.containsMouse ? root.tintOnSurface : "muted"
                }
                QbzSpinner {
                    visible: popRow.cacheStatus === 1 || popRow.cacheStatus === 2
                    anchors.centerIn: parent
                    size: 16
                }
                QbzIcon {
                    visible: popRow.cacheStatus === 3
                    anchors.centerIn: parent
                    name: "circle-check-big"
                    width: 16
                    height: 16
                    tintName: "accent"
                }
                QbzIcon {
                    // Never alongside the pulled-track alert in the number
                    // cell: one row, one alert.
                    visible: popRow.cacheStatus === 4 && !popRow.pulledDead
                    anchors.centerIn: parent
                    name: "triangle-alert"
                    width: 16
                    height: 16
                    tintName: "favorite"
                }
                MouseArea {
                    id: popDlArea
                    anchors.fill: parent
                    enabled: !popRow.selectMode && !popRow.pulledDead
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        if (popRow.cacheStatus === 3) QbzPlayer.uncacheTrack(row.id)
                        else QbzPlayer.cacheTrack(row.id)
                    }
                }
            }
            // ⋯ row menu. It used to be a hover-lit button with NO handler at
            // all — a control that renders and no-ops, which is the defect
            // class this round is closing.
            Rectangle {
                width: cols.colMenu
                height: cols.colMenu
                radius: theme.radiusSm
                anchors.verticalCenter: parent.verticalCenter
                color: moreArea.containsMouse ? theme.surfaceElevated : "transparent"
                QbzIcon { anchors.centerIn: parent; name: "ellipsis"; width: 16; height: 16; tintName: moreArea.containsMouse ? root.tintOnSurface : "muted" }
                MouseArea {
                    id: moreArea
                    anchors.fill: parent
                    enabled: !popRow.selectMode
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: function (mouse) { popMenu.openAtCursor(moreArea, mouse.x, mouse.y) }
                }
            }
        }

        // primitives/TrackContextMenu.slint, in ITS order, restricted to the
        // entries whose seam is live at this call site.
        //
        // REUSE, and why it is not `rows/TrackRow.qml`: PopularTrackRow is a
        // pre-existing, documented fork (see the component header) and the
        // fork is load-bearing here — its heart reads the VIEW-level
        // optimistic store (`root.toggleState("track:" + id, …)`, shared with
        // the header and the library tab) while TrackRow owns a private
        // `favorite` property, and its play route is the artist-context
        // `playArtistTrack`. Swapping the component is a view rewrite, not a
        // menu fix. What IS reused is the menu surface itself:
        // `controls/CardMenu.qml`, the same primitive rows/TrackRow.qml opens.
        //
        // ABSENT, not dead (the same discipline TrackRow applies): the radio
        // pair and Share Qobuz link / Song.link, which need bridge seams that
        // do not exist. Track info IS wired (2026-09-13) through ONE lazy
        // TrackInfoModal per artist view (`artistTrackInfo` below), not one
        // per row — the per-row Loader rows/TrackRow.qml carries is what this
        // comment used to refuse to duplicate.
        CardMenu {
            id: popMenu
            menuWidth: 224
            entries: {
                var t = QbzSession.tr
                var r = QbzSession.trRev
                var fav = root.toggleState("track:" + popRow.row.id, popRow.row.isFavorite)
                var m = []
                // A DEAD row gets no transport and no favourite entries: D5
                // keeps an unavailable track out of the queue at the seam, so
                // "Play next" here would render and no-op — the
                // rendered-and-inert defect this menu's own header refuses
                // ("ABSENT, not dead"). The row keeps its menu because the
                // rest of it (go-to) still works and because a right-clickable
                // dead row is what §5.2 asks for.
                if (!popRow.pulledDead) {
                    m.push({ "label": t("Play now", r), "icon": "play-fill", "action": "play" })
                    m.push({ "label": t("Play next", r), "icon": "list-start", "action": "next" })
                    m.push({ "label": t("Play later", r), "icon": "list-plus", "action": "later" })
                    m.push({ "label": t("Add to queue", r), "icon": "list-end", "action": "queue" })
                    m.push({ "label": fav ? t("Remove from Library", r) : t("Add to Library", r),
                             "icon": fav ? "heart-filled" : "heart", "action": "favorite" })
                }
                // A pulled track is refused BOTH containers, and on `pulled`
                // rather than `pulledDead`: a downloaded copy plays for this
                // user on this machine, but what a playlist or a mixtape stores
                // is the Qobuz catalog id, which resolves to nothing everywhere
                // else. Seeding a dead row into a collection is the one thing
                // worth refusing outright.
                if (!popRow.pulled) {
                    m.push({ "label": t("Add to mixtape", r), "icon": "cassette-tape", "action": "mixtape" })
                    m.push({ "label": t("Add to playlist", r), "icon": "list-music", "action": "add-to-playlist" })
                }
                // The offline block goes with the transport, and for the same
                // reason: a pulled track has no stream url left, so the
                // download arm would fail and a REFRESH would destroy the only
                // copy the user still has.
                if (!popRow.pulledDead) {
                    if (popRow.cacheStatus === 3) {
                        // `popRow.pulled` and NOT `pulledDead` here: this arm
                        // IS the cached-but-pulled row (pulled + cacheStatus 3
                        // is exactly what `pulledDead` excludes). A refresh on
                        // it would re-fetch a url that no longer exists and end
                        // with the copy gone — the one copy still playing.
                        // Remove stays: the disk space is the user's call.
                        if (!popRow.pulled)
                            m.push({ "label": t("Refresh offline copy", r), "icon": "refresh-cw", "action": "recache" })
                        m.push({ "label": t("Remove offline copy", r), "icon": "trash-2", "action": "uncache", "danger": true })
                    } else {
                        m.push({ "label": t("Make available offline", r), "icon": "cloud-download", "action": "cache" })
                    }
                }
                if ((popRow.row.albumId || "") !== "")
                    m.push({ "label": t("Go to album", r), "icon": "disc-3", "action": "go-album" })
                if ((popRow.row.artistId || "") !== "")
                    m.push({ "label": t("Go to artist", r), "icon": "user", "action": "go-artist" })
                if (!popRow.pulled)
                    m.push({ "label": t("Track info", r), "icon": "info", "action": "track-info" })
                return m
            }
            onPicked: function (a) {
                var id = popRow.row.id
                if (a === "play") popRow.play()
                else if (a === "next") QbzPlayer.enqueueTrack(id, "next")
                else if (a === "later") QbzPlayer.enqueueTrack(id, "later")
                else if (a === "queue") QbzPlayer.enqueueTrack(id, "queue")
                else if (a === "favorite") {
                    root.setToggleState("track:" + id,
                        !root.toggleState("track:" + id, popRow.row.isFavorite))
                    QbzLibrary.libraryToggleFavorite("track", id)
                }
                else if (a === "add-to-playlist") QbzPlaylistPicker.openForTrack(id)
                else if (a === "mixtape") {
                    // The HOST builds the AddItem payload. SOURCE: every row
                    // this component draws is a Qobuz catalog track — the
                    // `/artist/page` `top_tracks` / `appears_on` lists, and
                    // the in-library tab's rows, which artist_qt maps from
                    // Qobuz `Track` values. There is no local artist page.
                    QbzMyQbzAdd.open(JSON.stringify([{
                        "itemType": "track", "source": "qobuz",
                        "sourceItemId": id, "title": popRow.row.title || "",
                        "subtitle": popRow.row.artist || "", "artworkUrl": popRow.row.artUrl || "",
                        "year": null, "trackCount": null
                    }]))
                }
                else if (a === "cache") QbzPlayer.cacheTrack(id)
                else if (a === "uncache") QbzPlayer.uncacheTrack(id)
                else if (a === "recache") QbzPlayer.recacheTrack(id)
                else if (a === "go-album") QbzAlbum.openAlbum(popRow.row.albumId)
                else if (a === "go-artist") QbzArtist.openArtist(popRow.row.artistId)
                else if (a === "track-info") root.openTrackInfo(id)
            }
        }

        MouseArea {
            id: trArea
            anchors.fill: parent
            hoverEnabled: true
            propagateComposedEvents: true
            // The dead row is inert on double-click too (D4): no request, no
            // toast, no error. The right-press area below is untouched, so the
            // menu still opens.
            onDoubleClicked: if (!popRow.selectMode && !popRow.pulledDead)
                popRow.play()
            onClicked: {
                if (popRow.selectMode) popRow.toggleSelect(mouse.modifiers)
                else mouse.accepted = false
            }
        }
        // Right press opens the SAME menu at the pointer — the invariant
        // controls/QbzContextMenu.qml:20-22 states. Declared last so it sits
        // on top, RIGHT-only so every left click still falls through.
        MouseArea {
            id: popRcArea
            anchors.fill: parent
            acceptedButtons: Qt.RightButton
            onClicked: function (mouse) { popMenu.openAtCursor(popRcArea, mouse.x, mouse.y) }
        }
    }

    // Sidebar link row (SidebarLink). `navigable` false = informational row:
    // no pointer cursor, no hover promise it cannot keep (used by the MB
    // Relationships rows, which have no destination in this port).
    component SidebarLink: Rectangle {
        property string label: ""
        property string iconName: "user"
        property string tooltip: ""
        property bool navigable: true
        signal clicked()
        width: parent ? parent.width : 0
        height: 28
        radius: 4
        color: slArea.containsMouse ? theme.surfaceElevated : "transparent"
        Row {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            spacing: 8
            QbzIcon {
                name: iconName
                width: 12
                height: 12
                anchors.verticalCenter: parent.verticalCenter
                tintName: slArea.containsMouse ? root.tintOnSurface : "muted"
            }
            Text {
                width: parent.width - 20
                anchors.verticalCenter: parent.verticalCenter
                text: label
                color: slArea.containsMouse ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 13
                elide: Text.ElideRight
            }
        }
        MouseArea {
            id: slArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: navigable ? Qt.PointingHandCursor : Qt.ArrowCursor
            onClicked: if (navigable) parent.clicked()
            ToolTip.visible: containsMouse && tooltip !== ""
            ToolTip.text: tooltip
            ToolTip.delay: 400
        }
    }

    // Sidebar section heading (11px muted caps, letter-spaced).
    component SidebarSectionHeading: Text {
        color: theme.textMuted
        font.pixelSize: 11
        font.weight: theme.weightSemibold
        font.letterSpacing: 0.5
    }

    // Small 11px muted line — sub-group labels and the empty states inside
    // the sidebar sections. (The "Loading…" lines are now SidebarSkeleton.)
    component SidebarNote: Text {
        color: theme.textMuted
        font.pixelSize: 12
    }

    // Placeholder rows for a sidebar section still in flight — the shared
    // QbzSkeleton at the 28px pitch of SidebarLink, so the section holds its
    // band and nothing jumps when the real links land.
    //
    // `phase` is a property, not a file-scope id lookup: an inline `component`
    // does not see the enclosing document's ids (QbzSkeleton.qml's gotcha), so
    // the host passes the one shared timer in.
    component SidebarSkeleton: Column {
        id: sbSk
        property bool phase: false
        property int rows: 3
        // -28 = the section Column's left+right padding (see OriginRow).
        width: parent ? parent.width - 28 : 0
        spacing: 9
        Repeater {
            model: sbSk.rows
            delegate: QbzSkeleton {
                required property int index
                variant: "block"
                width: sbSk.width * (index % 2 === 0 ? 0.86 : 0.6)
                height: 13
                cellIndex: index
                phase: sbSk.phase
            }
        }
    }

    // One "KEY   value" row of the MB Origin block.
    component OriginRow: Item {
        id: originRow
        property string key: ""
        property string value: ""
        /// When true the value renders as a link and emits `activated`.
        /// Only the location row ever sets it (door D1 to the Artist Scene);
        /// born/founded/died/disbanded stay plain text, as in the reference.
        property bool clickable: false
        signal activated()
        // The host section Column carries 14px of left+right padding, and a
        // QML Positioner does NOT shrink its children for it — a right-aligned
        // value bound to the bare parent.width would run past the sidebar
        // edge. Subtract it here (this row only ever lives in that section).
        width: parent ? parent.width - 28 : 0
        height: 20
        Text {
            id: originKey
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            text: key
            color: theme.textMuted
            font.pixelSize: 11
            font.weight: theme.weightSemibold
            font.letterSpacing: 0.5
        }
        Text {
            id: originValue
            anchors.right: parent.right
            anchors.left: originKey.right
            anchors.leftMargin: 8
            anchors.verticalCenter: parent.verticalCenter
            text: value
            // The reference's `.origin-location-link` is
            // `background:none; border:none; padding:0; font-size:13px;
            // color:var(--accent-primary)` with `text-decoration:underline;
            // text-underline-offset:2px` on hover
            // (ArtistDetailView.svelte:3365-3379). Same 13px as the plain
            // rows, so only the colour and the underline change.
            color: originRow.clickable ? theme.accent : theme.textPrimary
            font.pixelSize: 13
            font.underline: originRow.clickable && originLink.containsMouse
            horizontalAlignment: Text.AlignRight
            elide: Text.ElideRight
        }
        MouseArea {
            id: originLink
            // Only the text, not the whole row — the key ("BORN IN") is not
            // part of the link in the reference either.
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            width: Math.min(originValue.implicitWidth, originValue.width)
            enabled: originRow.clickable
            visible: originRow.clickable
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: originRow.activated()
        }
    }

    // One MB relationship sub-group (Members & Former / Member Of /
    // Collaborators) with a preview cap + expander.
    component RelationshipGroup: Column {
        id: relGroup
        property string title: ""
        property var rows: []
        property string iconName: "user"
        /// The MusicBrainz role this group represents ("member", "producer",
        /// …) — passed to the resolver so a same-name match in another role is
        /// not treated as this musician.
        property string roleKey: ""
        property bool expanded: false
        signal toggled()
        visible: rows.length > 0
        // -28 = the host section Column's left+right padding (see OriginRow).
        width: parent ? parent.width - 28 : 0
        spacing: 2
        topPadding: 2
        SidebarNote {
            text: relGroup.title
            font.pixelSize: 11
        }
        Repeater {
            model: relGroup.rows.length > root.sidebarPreview && !relGroup.expanded
                   ? relGroup.rows.slice(0, root.sidebarPreview)
                   : relGroup.rows
            delegate: SidebarLink {
                required property var modelData
                label: modelData.name
                tooltip: modelData.tooltip
                iconName: relGroup.iconName
                // Relationship rows carry a NAME, not a catalog id, so the
                // click resolves through MusicBrainz first. Only a confirmed
                // match navigates (resolve_musician logs and stays put
                // otherwise) — landing the user on a same-name artist is worse
                // than the row doing nothing.
                navigable: true
                // `modelData.role`, NOT `relGroup.roleKey`. Rust already built
                // the right string per ROW: `group_relations` seeds each group
                // with the reference's own default ("Band Member" / "Band" /
                // "Collaborator") and then prefers that person's actual first
                // credited role (artist_qt.rs `map_relationships`), which is
                // exactly what the reference sends
                // (ArtistDetailView.svelte:3015,3033,3046). Sending `roleKey`
                // instead threw all of that away and shipped a per-GROUP
                // lowercase MusicBrainz key shared by every row — and for the
                // middle group it is not even a casing difference, it sent
                // "member of" where the reference sends "Band". The role is
                // echoed back on every appearance card, so it was visible.
                onClicked: QbzArtist.resolveMusician(modelData.name,
                                                     modelData.role || relGroup.roleKey || "")
            }
        }
        Text {
            visible: relGroup.rows.length > root.sidebarPreview
            leftPadding: 8
            // Same msgid pair the page's other expanders use — no new
            // catalog entries (all 8 locales already carry these).
            text: relGroup.expanded
                  ? QbzSession.tr("View less", QbzSession.trRev)
                  : QbzSession.tr("Load more", QbzSession.trRev)
            color: relMoreArea.containsMouse ? theme.textPrimary : theme.textSecondary
            font.pixelSize: 12
            MouseArea {
                id: relMoreArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: relGroup.toggled()
            }
        }
    }

    // Release section (ReleaseGrid).
    component ReleaseSection: Column {
        // Named because the card delegate below reaches for `section` from a
        // nested scope, and an inline component's own root id is the one
        // handle that is unambiguous there.
        id: relSection
        property var section: ({})
        property string anchorId: ""
        width: parent ? parent.width : 0
        spacing: 12

        Row {
            width: parent.width
            spacing: 12
            Text {
                width: parent.width - seeAll.width - sortBtn.width - playBtn.width - 36
                anchors.verticalCenter: parent.verticalCenter
                text: section.title
                color: theme.textPrimary
                font.pixelSize: theme.fontHeading
                font.weight: theme.weightSemibold
                elide: Text.ElideRight
            }
            Text {
                id: seeAll
                anchors.verticalCenter: parent.verticalCenter
                height: 28
                text: QbzSession.tr("See discography", QbzSession.trRev)
                color: seeAllArea.containsMouse ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 13
                verticalAlignment: Text.AlignVCenter
                MouseArea {
                    id: seeAllArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    // ReleaseGrid.slint:63-68 -> ArtistPageView.slint:928-929
                    // -> ArtistActions.open-releases -> main.rs:15055-15087,
                    // which records the nav entry and mounts
                    // ArtistReleasesView on this bucket.
                    //
                    // The artist NAME travels with the call: the second door to
                    // the same page (AlbumView's "From the same artist" View
                    // all) has no artist document open and supplies its own,
                    // exactly as main.rs:11844 does.
                    //
                    // `relSection.section`, not a bare `section` — the inline
                    // component's own root id is the one unambiguous handle
                    // from a nested scope, which is why `relSection` is named
                    // at all (:1409-1411). `root.` is the same pattern the card
                    // delegate below already uses for `coverMap` and
                    // `releaseFadeFrom`.
                    //
                    // (What stood here was an empty MouseArea under a POC-NOTE
                    // reading "dedicated discography page out of scope" — a
                    // fully lit, pointer-cursor control whose entire effect was
                    // nothing. The page now exists; the note is gone with it.)
                    onClicked: QbzArtist.openReleases(root.artist.id || "",
                        root.artist.name || "",
                        relSection.section.releaseType || "")
                }
            }
            // Per-section sort — ReleaseGrid.slint:70-90, the SAME control the
            // reference mounts (primitives/QbzSelect.slint), here through the
            // port's existing replica controls/QbzSelect.qml.
            //
            // `sm: true` + `menuWidth: 150` are the .slint's own values,
            // verbatim: QbzSelect.qml:34 is `width = menuWidth - 40` under
            // `sm`, so 150 lands at the 110 x 30 the reference computes at
            // QbzSelect.slint:88. Passing 190 "to get 150" is the exact mistake
            // that file's :42-50 note documents.
            //
            // `id: sortBtn` is load-bearing beyond this block — the section
            // title above measures `parent.width - seeAll.width - sortBtn.width
            // - 24`. The control grows 28 -> 30 tall with the swap; that IS the
            // reference (QbzSelect.slint `sm` is 30px). Row children are
            // top-aligned, hence the explicit verticalCenter.
            //
            // (What stood here was a dimmed Rectangle with no MouseArea, whose
            // note claimed `set-section-sort` "re-sorts server-side" and that
            // no seam existed. Both halves were wrong: main.rs:14997-15007 sorts
            // the ALREADY-LOADED cards in process and never touches the API —
            // see artist_qt::resort_section — and the label lied twice on top of
            // that, reading "Newest" over data in server order.)
            QbzSelect {
                id: sortBtn
                anchors.verticalCenter: parent.verticalCenter
                sm: true
                menuWidth: 150
                // The .slint's five options in its order (:74-79). "A–Z"/"Z–A"
                // carry a U+2013 EN DASH — the msgid in the catalogue is the
                // en-dash form, and a hyphen would silently go untranslated in
                // all seven non-English locales. (LibraryToolbar's "Title A-Z"
                // is a DIFFERENT, hyphenated msgid — do not copy it here.)
                options: [QbzSession.tr("Default", QbzSession.trRev),
                          QbzSession.tr("Newest", QbzSession.trRev),
                          QbzSession.tr("Oldest", QbzSession.trRev),
                          QbzSession.tr("A–Z", QbzSession.trRev),
                          QbzSession.tr("Z–A", QbzSession.trRev),
                          QbzSession.tr("Popularity", QbzSession.trRev)]
                // ReleaseGrid.slint:34-39 — the wire key back to an index. This
                // is what makes the picker come back showing the sort the user
                // actually chose: `sortBy` is stamped on the section by
                // artist_qt.rs at page build from the persisted pref, so a
                // revisited artist seats the control without a click.
                //
                // A BINDING, never an assignment: QbzSelect does not
                // self-assign `currentIndex` (it closes the popup and emits,
                // QbzSelect.qml:299-300), and every enrichment republish
                // re-creates this delegate — an assignment would be lost.
                currentIndex: {
                    var s = relSection.section.sortBy || "default"
                    return s === "newest" ? 1 : s === "oldest" ? 2
                         : s === "title-asc" ? 3 : s === "title-desc" ? 4 : s === "relevant" ? 5 : 0
                }
                // ReleaseGrid.slint:81-89 — index to wire key. These five
                // strings are BOTH what gets persisted and what the sort
                // functions switch on, on either side of the bridge.
                //
                // `relSection.section`, not a bare `section`: this header lives
                // inside an inline `component`, which does not see the enclosing
                // document's ids — `relSection` exists for exactly this.
                onSelected: function (i) {
                    root.releaseSortChanged(relSection.section.releaseType,
                        i === 1 ? "newest" : i === 2 ? "oldest"
                        : i === 3 ? "title-asc" : i === 4 ? "title-desc" : i === 5 ? "relevant" : "default")
                }
            }
            // Play all / Play random / Play selected over THIS bucket, in
            // its sort order (`section.cards` is already sorted — Rust sorts
            // the bucket and the overlay merge re-sorts).
            QbzSplitButton {
                id: playBtn
                anchors.verticalCenter: parent.verticalCenter
                label: root.albumSectionLabel(relSection.section.releaseType || "")
                menuItems: root.albumSectionMenu(relSection.section.releaseType || "")
                onClicked: root.albumSectionPrimary(relSection.section.releaseType || "",
                                                    relSection.section.cards || [])
                onPicked: function (a) {
                    root.albumSectionAction(relSection.section.releaseType || "",
                                            relSection.section.cards || [], a)
                }
            }
        }

        Grid {
            width: parent.width
            columns: Math.max(1, Math.floor((width + 24) / 224))
            columnSpacing: 24
            rowSpacing: 24
            Repeater {
                model: section.cards
                delegate: AlbumCard {
                    id: relCard
                    albumId: modelData.id
                    title: modelData.title
                    // The subtitle slot carries the YEAR on the artist page,
                    // not the artist: artist.rs `card_to_item` (:670-688)
                    // re-routes `year` through the card's `artist` field
                    // precisely so the shared card primitive stays unchanged
                    // ("the artist is redundant since we're already on their
                    // page"), and blanks artist_id so the line is inert.
                    artist: modelData.year
                    artistId: ""
                    genre: modelData.genre
                    year: modelData.year
                    qualityTier: modelData.qualityTier
                    qualityDetail: modelData.qualityDetail || ""
                    artSource: root.coverMap[modelData.artUrl] || ""
                    // artist_qt `map_release` stamps the pin state on every
                    // release row; SectionRail is the only other reader and
                    // this page never mounts it, so the flag was published
                    // and dropped on the floor — the glyph lied on all four
                    // of this page's album grids. `artUrl` is the REMOTE url
                    // (coverMap is keyed BY it), which is what the pin
                    // payload must store.
                    isPinned: modelData.isPinned === true
                    artworkUrl: modelData.artUrl || ""
                    // artist_qt::map_release stamps this the same way it
                    // stamps the pin; false inverted the first click.
                    isFavorite: modelData.isFavorite === true
                    // "Play selected" over this bucket (the split button).
                    selectMode: root.albumSelectKey === (relSection.section.releaseType || "")
                    selected: root.albumSelected[modelData.id] === true
                    selectMarkBottom: true
                    onSelectToggled: root.albumSelectToggle(modelData.id)

                    // ---- smooth append (owner, 2026-08-02) ---------------
                    // "que la aparicion de lo que se cargue, sea smooth" —
                    // fade in ONLY the page Load more just brought in. The
                    // Repeater re-creates EVERY delegate on any republish (see
                    // `releasePending` at the top of this file), so an
                    // unconditional fade would re-dissolve the whole grid on
                    // an unrelated pass and read as a flicker.
                    //
                    // `opacity` carries NO binding on purpose: the assignment
                    // below would destroy one, and a Behavior would then fire
                    // for every card (Behaviors are inert only DURING
                    // creation, and Component.onCompleted runs after it). A
                    // card below the threshold is never touched at all, so it
                    // is created — and stays — fully opaque.
                    NumberAnimation {
                        id: appendFade
                        target: relCard
                        property: "opacity"
                        from: 0.0
                        to: 1.0
                        // 220ms / OutCubic is the content duration; the
                        // skeleton above it fades at 180ms.
                        duration: 220
                        easing.type: Easing.OutCubic
                    }
                    Component.onCompleted: {
                        if (index >= root.releaseFadeFrom(relSection.section.releaseType)) {
                            relCard.opacity = 0.0
                            appendFade.start()
                        }
                    }
                }
            }
        }

        // The shared affordance (controls/QbzLoadMore.qml) — this site is the
        // `a` arm its header cites: plain centred text, 28 tall, and the same
        // `section.hasMore` gate and bridge call as before. What is new is the
        // placeholder row it draws underneath while the page is in flight.
        QbzLoadMore {
            visible: section.hasMore || section.sortLoading === true
            width: parent.width
            // The grid above has a 224 x 270 PITCH: cards/AlbumCard.qml:186-189
            // is 200 x 246 and the Grid adds 24px of column/row spacing — the
            // same pitch SearchView's block was built around, so the
            // placeholder lands exactly where the first appended row will.
            skeleton: "cards"
            cellW: 224
            cellH: 270
            // There is NO per-section loading flag to bind to: the bridge
            // publishes one artist document plus the `releaseSectionReady`
            // signal (artist_bridge.rs:60-66) and nothing in between. So the
            // in-flight state is driven locally, off root's map, cleared by
            // that signal and capped by root's 8s settle timer (the error path
            // emits nothing at all — main.rs:872).
            busy: section.sortLoading === true || root.releasePending[section.releaseType] === true
            onClicked: {
                // BEFORE the call: the threshold is the count on screen now
                // (`section.cards` is the MERGED array — document page 1 plus
                // every page the overlay holds — so the fade threshold lines
                // up with the tail this request will add).
                //
                // The OFFSET is not that count: releaseMoreRequested returns
                // the server cursor (see `releaseCursor`), which is the number
                // of rows the server has actually sent. They differ as soon as
                // a page repeats a row, and passing the on-screen count then
                // re-requests rows we already have — the dedup swallows them
                // and the button appears to do nothing, forever.
                //
                // The third argument suppresses the append fade when this
                // bucket carries a non-default sort: the page will be
                // INTERLEAVED, not appended, so an index threshold would fade
                // the wrong cards (see releaseMoreRequested).
                var off = root.releaseMoreRequested(section.releaseType, section.cards.length,
                                                    (section.sortBy || "default") !== "default")
                QbzArtist.loadReleaseSection(artist.id, section.releaseType, off)
            }
        }
    }

    // ============================ the page ================================
    // FULL WIDTH even with the sidebar open. The .slint reserves the 300px
    // inside the BODY ROW only (ArtistPageView.slint:1094) — the header and
    // the JUMP TO bar span the whole window so the gradient covers edge to
    // edge (:184-191). Narrowing the whole Flickable (the port's old
    // `anchors.rightMargin`) squeezed the portrait and the bio too.
    Flickable {
        id: flick
        anchors.fill: parent
        clip: true
        contentWidth: width
        contentHeight: page.implicitHeight
        boundsBehavior: Flickable.StopAtBounds

        // Artwork-tinted header band — the SAME shared component AlbumView
        // mounts. First child = painted under the page; inside the Flickable
        // = scrolls with it (ArtistPageView.slint:213); full-bleed.
        HeaderGradient {
            x: 0
            y: 0
            width: flick.width
            // .slint:147 `atmo-height: page.y + body-row.y` — the band ends
            // exactly where the body begins, i.e. at the JUMP TO strip, so a
            // long bio pushes the fade down with no manual tuning.
            // `jumpSlot`, not `jumpBar`: the bar is a pinned overlay now and
            // its y is the CLAMPED viewport position, which would freeze this
            // band's height the moment the bar stuck.
            height: page.y + jumpSlot.y
            tint: artist.headerColor || ""
            // Route A: the blurred field. Empty until the cover resolves, and the
            // flat tint stands in meanwhile (HeaderGradient handles the swap).
            atmosphere: artist.headerAtmosphere || ""
            active: root.headerAtmoOn
        }

        Column {
            id: page
            width: parent.width
            leftPadding: 32
            rightPadding: 32
            topPadding: 11
            bottomPadding: 100
            spacing: 0

            // Width available to the BODY sections: the page width less the
            // 32+32 padding, less the sidebar reservation while it is open
            // (.slint's empty 300px slot in the body row). The header and the
            // jump strip deliberately do NOT subtract it.
            readonly property real bodyWidth: width - 64 - (root.networkOpen ? 300 : 0)

            Item { width: 1; height: 22 }

            // --- Artist header skeleton ----------------------------------
            // Mounted on the primary flag, and the real header is hidden by
            // the same flag: opening artist B never renders a half-empty
            // header frame while B's document is in flight.
            Row {
                visible: root.primaryLoading
                width: parent.width - 64
                spacing: 32

                QbzSkeleton { variant: "circle"; width: 200; height: 200; phase: skeletonPhase.on }
                Column {
                    width: parent.width - 200 - 32
                    spacing: 12
                    Item { width: 1; height: 10 }
                    QbzSkeleton { variant: "block"; width: Math.min(360, parent.width); height: 30; cellIndex: 0; phase: skeletonPhase.on }
                    QbzSkeleton { variant: "block"; width: Math.min(520, parent.width); height: 14; cellIndex: 1; phase: skeletonPhase.on }
                    QbzSkeleton { variant: "block"; width: Math.min(440, parent.width); height: 14; cellIndex: 2; phase: skeletonPhase.on }
                    Item { width: 1; height: 14 }
                    Row {
                        spacing: 12
                        Repeater {
                            model: 4
                            delegate: QbzSkeleton {
                                required property int index
                                variant: "circle"
                                width: 44
                                height: 44
                                cellIndex: index
                                phase: skeletonPhase.on
                            }
                        }
                    }
                }
            }

            // --- Artist header ------------------------------------------
            Row {
                visible: !root.primaryLoading
                width: parent.width - 64
                spacing: 32

                // Circular portrait. The circle comes from RoundedImage's
                // MASK, not from a clip: QML's `clip` is rectangular and does
                // NOT follow a Rectangle's radius — theme/RoundedImage.qml:3-6
                // says so and proved it with an isolated scene on this Qt
                // build, and :508 measures this exact radius-100-on-200px
                // case. The `clip: true` that used to sit here (with a comment
                // claiming the opposite) therefore rounded nothing, cost an
                // unconditional batch root, and would have swallowed anything
                // mounted inside the frame — which is why the menu and the
                // lightbox are view-root siblings, exactly as
                // AlbumView.qml:586-588 keeps them.
                Rectangle {
                    width: 200
                    height: 200
                    radius: 100
                    color: theme.surfaceElevated
                    RoundedImage {
                        anchors.fill: parent
                        // A custom portrait (the SHARED custom_artwork store,
                        // keyed by artist NAME) beats the url-keyed pipeline
                        // image — the album header's rule on the artist axis.
                        source: (artist.customImageUrl || "") !== ""
                            ? artist.customImageUrl
                            : (root.coverMap[artist.artUrl] || "")
                        radius: 100
                    }
                    // Left-click: the lightbox (NEW in this port — the
                    // reference lets left clicks pass through,
                    // ArtistPageView.slint:290-293). Right-click: the portrait
                    // menu the reference DOES have (ArtistPageView.slint:292-353),
                    // which this port simply never paid.
                    MouseArea {
                        anchors.fill: parent
                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                        cursorShape: Qt.PointingHandCursor
                        onClicked: function (mouse) {
                            if (mouse.button === Qt.RightButton)
                                portraitMenu.openAtCursor(portraitMenuAnchor, mouse.x, mouse.y)
                            else if (root.bestArtistImage() !== "")
                                portraitLightbox.openWith(root.bestArtistImage())
                        }
                    }
                    Item { id: portraitMenuAnchor; anchors.fill: parent }
                }

                Column {
                    width: parent.width - 200 - 32
                    anchors.top: parent.top
                    anchors.topMargin: 8
                    spacing: 0

                    Text {
                        width: parent.width
                        text: artist.name || ""
                        color: root.hdrStrong
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightBold
                        elide: Text.ElideRight
                    }

                    Item { visible: (artist.bio || "") !== ""; width: 1; height: 12 }
                    Text {
                        visible: (artist.bio || "") !== ""
                        width: parent.width
                        text: artist.bioShort || ""
                        color: root.hdrBody
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                    Item { visible: artist.bioTruncated === true; width: 1; height: 4 }
                    Text {
                        visible: artist.bioTruncated === true
                        text: QbzSession.tr("Read more", QbzSession.trRev)
                        color: readMoreArea.containsMouse ? theme.accentHover : theme.accent
                        font.pixelSize: theme.fontLegal
                        MouseArea {
                            id: readMoreArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                var shell = root.parent
                                while (shell && shell.openTextModal === undefined) shell = shell.parent
                                if (!shell) return
                                // The Slint modal renders the attribution
                                // ("Source: TiVo") as a small line under the
                                // body; the shared text modal has one body
                                // slot, so it rides at the end.
                                var body = artist.bio || ""
                                if ((artist.bioSource || "") !== "")
                                    body += "\n\n" + QbzSession.tr("Source", QbzSession.trRev) + ": " + artist.bioSource
                                shell.openTextModal(artist.name || "", body)
                            }
                        }
                    }

                    Item { width: 1; height: 18 }
                    // Action row — ArtistPageView.slint:413-591. Four
                    // circles (NO Play: Popular Tracks carries its own), then
                    // a stretch, then the catalog/library toggle floated
                    // right. The palette arm follows the header backdrop
                    // (`on-surface: root.hdr-on-surface`, :417).
                    Row {
                        width: parent.width
                        spacing: 12
                        QbzCircleAction {
                            readonly property bool following: root.toggleState("artist", artist.isFollowing)
                            name: following ? "heart-filled" : "heart"
                            overlay: root.hdrOverlay
                            active: following
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: {
                                root.setToggleState("artist", !following)
                                QbzLibrary.libraryToggleFavorite("artist", artist.id)
                            }
                        }
                        // Radio — ArtistPageView.slint:426-461: the disc opens
                        // a two-row dropdown, QBZ Radio (the local `qbz-radio`
                        // pool builder) vs Qobuz Radio (the curated
                        // `/radio/artist` endpoint).
                        //
                        // IT WAS `btnEnabled: false` — dimmed and inert by
                        // declaration, because at the time neither engine had a
                        // seam on a Qt bridge and the port refuses to render a
                        // control that drives nothing. Both exist now:
                        // `QbzHome.startArtistRadio` (the smart pool, already
                        // shipping behind the Discover spotlight's RADIO card)
                        // and `QbzHome.startQobuzArtistRadio`, added with this
                        // change. So the disc goes live and the dropdown is the
                        // reference's, verbatim.
                        QbzCircleAction {
                            id: radioBtn
                            name: "radio"
                            overlay: root.hdrOverlay
                            // BOTH dropdown rows arm the same key: they are
                            // two ways to fill one queue from one disc, and
                            // the disc is what the user is looking at. The
                            // control also gates its own clicks while loading,
                            // so the dropdown cannot be re-opened over a
                            // request that is already running.
                            loading: QbzHome.radioPending === "artist:" + artist.id
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: radioMenu.openBelowLeft(radioBtn)
                        }
                        QbzCircleAction {
                            name: "element-connect"
                            overlay: root.hdrOverlay
                            active: root.networkOpen
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: root.networkOpen = !root.networkOpen
                        }
                        QbzCircleAction {
                            id: overflowBtn
                            name: "ellipsis"
                            overlay: root.hdrOverlay
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: function (mouse) { overflowMenu.openAtCursor(overflowBtn, mouse.x, mouse.y) }
                        }
                        // Stretch (.slint:579 `Rectangle { horizontal-stretch: 1 }`).
                        // Clamped: at a narrow window an unclamped negative
                        // width silently reflows the whole row.
                        Item {
                            width: Math.max(0, parent.width - 4 * 32 - 4 * 12
                                               - (segTabs.visible ? segTabs.width + 12 : 0))
                            height: 1
                        }
                        // From catalog / In library — .slint:582 mounts the
                        // SHARED SegmentedTabBar; the port hand-rolled a copy
                        // of it here whose delegate walked the wrong parent
                        // chain (`parent.parent.modelData` on a Row, and the
                        // count chip read `active` off the wrong node), so the
                        // count badge never took its active colours. This is
                        // the shared control (controls/QbzTabBar.qml), which
                        // is that same SegmentedTabBar 1:1 — counts on, and
                        // the 2px accent underline the .slint's Segment draws
                        // for the active tab (:86-93).
                        QbzTabBar {
                            id: segTabs
                            visible: (artist.libraryCount || 0) > 0
                            anchors.verticalCenter: parent.verticalCenter
                            counts: true
                            underline: true
                            activeId: root.artistTab
                            tabs: [
                                { "id": "catalog", "label": QbzSession.tr("From catalog", QbzSession.trRev), "count": 0 },
                                { "id": "library", "label": QbzSession.tr("In library", QbzSession.trRev), "count": artist.libraryCount || 0 },
                            ]
                            onSelected: function (id) { root.artistTab = id }
                        }
                    }
                }
            }

            // --- Hidden-artist banner (ArtistPageView.slint:595-660) ------
            // Only when the CURRENTLY displayed artist is blacklisted. The page
            // stays fully navigable — a direct fetch-by-id is never blocked
            // (.slint:595-599) — so this is an unblock affordance, not a lock.
            // Sits between the header and the body row, exactly where the
            // .slint puts it (after the header block at :594, before the body
            // row), with the .slint's own 16px spacer (:600).
            // Built inline rather than through controls/WarningBanner.qml: that
            // control has no action slot, and this banner's whole point is the
            // right-hand "Show artist" button.
            Item { visible: root.artistBlacklisted; width: 1; height: 16 }
            Rectangle {
                id: hiddenBanner
                visible: root.artistBlacklisted
                width: parent.width - 64
                // .slint `height: banner-row.preferred-height` where the row is
                // a HorizontalLayout with padding 12 (:602) — so 24 plus the
                // tallest child: the 16px glyph, the wrapped copy, or the 28px
                // button (:604, :620, :639).
                height: visible ? 24 + Math.max(16, bannerCopy.implicitHeight, 28) : 0
                radius: 8
                // LITERALS, not theme tokens: the .slint hardcodes both
                // (:596-599). theme.warningBg / warningBorder are a different
                // amber (#fbbf24-based) and would not match the reference.
                color: "#eab3081a"
                border.width: 1
                border.color: "#eab3084d"

                // 16x16 blind-eye (:606-611). The .slint tints it with the
                // literal #eab308; QbzIcon.tintName is a CLOSED vocabulary of
                // names with no #eab308 bake, so "warning" (theme.warning
                // #fbbf24) is the nearest available and the only theme-following
                // amber — spec 03 C6's default decision.
                QbzIcon {
                    id: bannerGlyph
                    name: "blind-eye"
                    width: 16
                    height: 16
                    tintName: "warning"
                    anchors.left: parent.left
                    anchors.leftMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                }
                // "Show artist" button (:634-659): width = label + 20, height
                // 28, radius 6, hover fill surface-elevated else transparent,
                // label accent -> accentHover on hover, 13 / semibold.
                Rectangle {
                    id: bannerBtn
                    width: bannerBtnLabel.implicitWidth + 20
                    height: 28
                    radius: 6
                    color: bannerBtnArea.containsMouse ? theme.surfaceElevated : "transparent"
                    anchors.right: parent.right
                    anchors.rightMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    Text {
                        id: bannerBtnLabel
                        anchors.centerIn: parent
                        text: QbzSession.tr("Show artist", QbzSession.trRev)
                        color: bannerBtnArea.containsMouse ? theme.accentHover : theme.accent
                        font.pixelSize: 13
                        font.weight: theme.weightSemibold
                    }
                    MouseArea {
                        id: bannerBtnArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        // Same seam as the menu row (:654-657) — one toggle.
                        onClicked: root.toggleBlacklist()
                    }
                }
                // Copy (:619-627): text-secondary, Typography.legal = 13,
                // word-wrap. The 10px gaps on both sides are the .slint's
                // HorizontalLayout spacing (:603).
                Text {
                    id: bannerCopy
                    anchors.left: bannerGlyph.right
                    anchors.leftMargin: 10
                    anchors.right: bannerBtn.left
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("This artist is hidden from discovery", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: 13
                    wrapMode: Text.WordWrap
                }
            }

            Item { width: 1; height: 20 }

            // --- JUMP TO bar ---------------------------------------------
            // The SHARED controls/QbzJumpNavBar (primitives/JumpNavBar.slint
            // 1:1). What the port drew by hand was the tab strip ONLY: no
            // "JUMP TO" caption, no bottom hairline, no search affordance,
            // 13px/medium type instead of the .slint's 15px/regular, and the
            // wrong three tab colours. padH 0 because this Column already
            // pads 32 — the .slint's own 32px padding lands the strip in the
            // same place.
            // The bar itself is an OVERLAY outside this Flickable (see
            // `jumpBar` after it) so it can pin to the top on scroll. What
            // stays in the flow is this slot, which reserves exactly its
            // height: without it the body would jump up by the bar's height
            // the moment the bar was lifted out.
            Item {
                id: jumpSlot
                width: parent.width - 64
                height: jumpBar.height
            }

            // --- Primary placeholder --------------------------------------
            // Same flag the spinner used, now in the shape of what is coming:
            // the Popular Tracks heading plus 5 rows at the PopularTrackRow
            // 50px pitch (the preview count), so nothing shifts on arrival.
            Column {
                visible: root.primaryLoading
                width: parent.width - 64
                spacing: 0

                QbzSkeleton { variant: "block"; width: 190; height: 22; phase: skeletonPhase.on }
                Item { width: 1; height: 18 }
                Repeater {
                    model: root.preview
                    delegate: Item {
                        required property int index
                        width: parent ? parent.width : 0
                        height: 50
                        QbzSkeleton {
                            anchors.verticalCenter: parent.verticalCenter
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.leftMargin: 12
                            anchors.rightMargin: 24
                            height: 40
                            variant: "row"
                            cellIndex: index
                            phase: skeletonPhase.on
                        }
                    }
                }
            }

            // ================= Catalog tab ================================
            Column {
                id: sectionAnchors
                visible: root.artistTab === "catalog" && !QbzArtist.artistLoading
                // Body width: yields the 300px the sidebar overlay occupies
                // (.slint's empty reservation slot, :1094) so the content is
                // never painted under the panel.
                width: page.bodyWidth
                spacing: 0

                // --- Popular Tracks -------------------------------------
                // Multi-select bulk bar — INLINE above the Popular Tracks
                // header (the Local Library album view's layout: bar in flow,
                // content below), covering this page's Qobuz track lists.
                QbzMultiSelectBar {
                    visible: root.multiSelect
                    width: parent.width
                    selectedCount: root.selectedCount
                    actions: [
                        { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                        { "id": "play-next", "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "danger": false, "needsSelection": true },
                        { "id": "play-later", "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "danger": false, "needsSelection": true },
                        { "id": "queue", "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "danger": false, "needsSelection": true },
                        { "id": "add-to-playlist", "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "danger": false, "needsSelection": true },
                        { "id": "add-to-mixtape", "label": QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev), "icon": "cassette-tape", "danger": false, "needsSelection": true },
                        { "id": "add-to-favorites", "label": QbzSession.tr("Add to Library", QbzSession.trRev), "icon": "heart", "danger": false, "needsSelection": true },
                        { "id": "make-offline", "label": QbzSession.tr("Make available offline", QbzSession.trRev), "icon": "cloud-download", "danger": false, "needsSelection": true },
                        { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true }
                    ]
                    onAction: function (id) { root.bulkAction(id) }
                }

                // Air between the sticky JUMP TO bar's rule and the row.
                Item { visible: topTracks.length > 0; width: 1; height: 8 }
                Row {
                    property string anchorId: "popular-tracks"
                    visible: topTracks.length > 0
                    width: parent.width
                    spacing: 12
                    Text {
                        width: parent.width - 32 - 32 - 32 - 3 * 12
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Popular Tracks", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                    }
                    // The SHARED CircleAction, in the same plain 32px outlined
                    // form as its two siblings (2026-09-13): the filled 44px
                    // accent disc the .slint carried here clashed with them
                    // and butted the JUMP TO bar's rule.
                    QbzCircleAction {
                        name: "play-fill"
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzPlayer.playArtistTop(false)
                    }
                    // Multi-select toggle — live since the bulk-bar port.
                    QbzCircleAction {
                        name: "square-check-big"
                        active: root.multiSelect
                        btnEnabled: root.topTracks.length > 0
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: root.setMultiSelect(!root.multiSelect)
                    }
                    QbzCircleAction {
                        id: topMenuBtn
                        name: "ellipsis"
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: function (mouse) {
                            topMenu.forLibrary = false
                            topMenu.openAtCursor(topMenuBtn, mouse.x, mouse.y)
                        }
                    }
                }
                Item { visible: topTracks.length > 0; width: 1; height: 10 }

                Repeater {
                    // Do not build the unrevealed tail. Numeric growth keeps
                    // the preview delegates and adds only the requested rows.
                    model: root.topTracksExpanded ? topTracks.length
                        : Math.min(topTracks.length, root.preview)
                    delegate: PopularTrackRow {
                        // Smooth reveal (owner, 2026-08-02). This expander is
                        // CLIENT-SIDE: numeric model growth now creates only
                        // the revealed tail. The former full count built every
                        // row up front and only flipped `visible`.
                        //
                        // OPACITY ONLY, deliberately: the row is 50 tall with
                        // a 36px cover and text anchored to its verticalCenter
                        // and it does NOT clip, so animating `height` would
                        // paint those children outside the row box and over
                        // its neighbours for the whole 220ms. Height still
                        // snaps (the space opens at once); the content
                        // dissolves into it. Same 220ms / OutCubic the
                        // appended release cards use.
                        //
                        // The gate is this explicit local `revealed`, not the
                        // item's effective visibility, so showing the catalog
                        // tab again does not re-fade surviving delegates.
                        property bool revealed: false
                        height: 50
                        opacity: revealed ? 1.0 : 0.0
                        Behavior on opacity { NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }
                        Component.onCompleted: revealed = true
                        row: topTracks[index]
                        rowIndex: index
                        showAlbum: true
                        selectMode: root.multiSelect
                        checked: root.selected[row.id] === true
                        onToggleSelect: function (mods) { root.toggleSelected(row.id, mods) }
                    }
                }
                Item { visible: topTracks.length > root.preview; width: 1; height: 4 }
                // The shared affordance — the `b` arm of controls/QbzLoadMore
                // .qml's header: same plain 28-tall shape, same label pair,
                // same client-side toggle. No `busy` and no skeleton: there is
                // no fetch, the reveal is instant (the rows above own the
                // smoothing).
                QbzLoadMore {
                    visible: topTracks.length > root.preview
                    width: parent.width
                    label: root.topTracksExpanded ? QbzSession.tr("View less", QbzSession.trRev) : QbzSession.tr("Load more", QbzSession.trRev)
                    onClicked: root.topTracksExpanded = !root.topTracksExpanded
                }

                // --- Latest release --------------------------------------
                Column {
                    property string anchorId: "about"
                    visible: !!root.artist.lastRelease
                    width: parent.width
                    spacing: 12
                    Item { width: 1; height: 32 }
                    Text {
                        text: QbzSession.tr("Latest release", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                    }
                    AlbumCard {
                        // EVERY read here must be `root.artist.lastRelease`:
                        // inside the AlbumCard instantiation the bare name
                        // `artist` resolves to the CARD'S OWN string property
                        // (a string has no .lastRelease → undefined → every
                        // binding fell back to ""), so the card rendered as an
                        // empty tile while the grids' modelData-driven cards
                        // were fine. Verified live 2026-08-10 (VNC drive).
                        albumId: root.artist.lastRelease ? root.artist.lastRelease.id : ""
                        title: root.artist.lastRelease ? root.artist.lastRelease.title : ""
                        // year in the subtitle slot — card_to_item again
                        // (artist.rs:784 maps last_release through it too).
                        artist: root.artist.lastRelease ? root.artist.lastRelease.year : ""
                        artistId: ""
                        genre: root.artist.lastRelease ? root.artist.lastRelease.genre : ""
                        year: root.artist.lastRelease ? root.artist.lastRelease.year : ""
                        qualityTier: root.artist.lastRelease ? root.artist.lastRelease.qualityTier : ""
                        qualityDetail: root.artist.lastRelease ? (root.artist.lastRelease.qualityDetail || "") : ""
                        artSource: root.artist.lastRelease ? (root.coverMap[root.artist.lastRelease.artUrl] || "") : ""
                        // Same row shape as the release grids (map_release).
                        isPinned: root.artist.lastRelease ? root.artist.lastRelease.isPinned === true : false
                        artworkUrl: root.artist.lastRelease ? (root.artist.lastRelease.artUrl || "") : ""
                        isFavorite: root.artist.lastRelease ? root.artist.lastRelease.isFavorite === true : false
                    }
                }

                // --- Release sections ------------------------------------
                Repeater {
                    // `visible: false` does not prevent delegate construction.
                    // Feed this arm only regular buckets so the collapsed
                    // Other bucket is not built once here and again below.
                    model: releaseSections.filter(function (s) {
                        return s.releaseType !== "other"
                    })
                    delegate: Column {
                        required property var modelData
                        width: parent ? parent.width : 0
                        spacing: 0
                        Item { width: 1; height: 32 }
                        ReleaseSection { section: modelData; anchorId: modelData.releaseType }
                    }
                }

                // --- Appears On -------------------------------------------
                Column {
                    property string anchorId: "appears-on"
                    visible: appearsOn.length > 0
                    width: parent.width
                    spacing: 0
                    Item { width: 1; height: 32 }
                    Text {
                        text: QbzSession.tr("Appears On", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                    }
                    Item { width: 1; height: 10 }
                    Repeater {
                        model: root.appearsOnExpanded ? appearsOn.length
                            : Math.min(appearsOn.length, root.preview)
                        delegate: PopularTrackRow {
                            // Same client-side reveal as Popular Tracks above
                            // — same component, same 50px pitch, same reason
                            // for fading opacity and letting height snap.
                            property bool revealed: false
                            height: 50
                            opacity: revealed ? 1.0 : 0.0
                            Behavior on opacity { NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }
                            Component.onCompleted: revealed = true
                            row: appearsOn[index]
                            rowIndex: index
                            showAlbum: false
                            selectMode: root.multiSelect
                            checked: root.selected[row.id] === true
                            onToggleSelect: function (mods) { root.toggleSelected(row.id, mods) }
                        }
                    }
                    Item { visible: appearsOn.length > root.preview; width: 1; height: 4 }
                    // Shared affordance, `b` arm again (client-side toggle).
                    QbzLoadMore {
                        visible: appearsOn.length > root.preview
                        width: parent.width
                        label: root.appearsOnExpanded ? QbzSession.tr("View less", QbzSession.trRev) : QbzSession.tr("Load more", QbzSession.trRev)
                        onClicked: root.appearsOnExpanded = !root.appearsOnExpanded
                    }
                }

                // --- Playlists --------------------------------------------
                // The reference mounts the SHARED playlist carousel here
                // (ArtistPageView.slint:985-995 -> discover/PlaylistCarousel
                // -> discover/PlaylistCard), so this page gets the same card
                // Home, Search, Browse, Label and the Library feed already
                // mount: body click opens, overlay play/favourite/⋯, pin
                // badge, context menu — and the banner rendered CONTAIN
                // instead of cropped 2.11:1 into a square.
                //
                // What stood here was a hand-rolled Rectangle delegate whose
                // MouseArea had no onClicked at all (only a cursor, which is
                // why it LOOKED clickable) and a RoundedImage with no `fit:`,
                // i.e. the default crop. Its POC-NOTE claimed there was no
                // playlist view yet; views/PlaylistView.qml has existed and
                // been routed for a long time.
                Item { visible: playlists.length > 0; width: 1; height: 32 }
                PlaylistRail {
                    visible: playlists.length > 0
                    width: parent.width
                    title: QbzSession.tr("Playlists", QbzSession.trRev)
                    items: playlists
                    coverMap: root.coverMap
                }

                // --- Other (collapsed) ------------------------------------
                Repeater {
                    model: releaseSections.filter(function (s) {
                        return s.releaseType === "other"
                    })
                    delegate: Column {
                        required property var modelData
                        width: parent ? parent.width : 0
                        spacing: 0
                        Item { width: 1; height: 32 }
                        Row {
                            width: parent.width
                            spacing: 8
                            Text {
                                width: parent.width - otherToggle.implicitWidth - 8
                                anchors.verticalCenter: parent.verticalCenter
                                text: modelData.title
                                color: theme.textPrimary
                                font.pixelSize: theme.fontHeading
                                font.weight: theme.weightSemibold
                            }
                            Text {
                                id: otherToggle
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.otherExpanded ? QbzSession.tr("Hide", QbzSession.trRev) : QbzSession.tr("Show", QbzSession.trRev)
                                color: otherToggleArea.containsMouse ? theme.textPrimary : theme.textSecondary
                                font.pixelSize: 13
                                MouseArea {
                                    id: otherToggleArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.otherExpanded = !root.otherExpanded
                                }
                            }
                        }
                        Item { visible: root.otherExpanded; width: 1; height: 12 }
                        Grid {
                            visible: root.otherExpanded
                            width: parent.width
                            columns: Math.max(1, Math.floor((width + 24) / 224))
                            columnSpacing: 24
                            rowSpacing: 24
                            Repeater {
                                // A hidden Grid still constructs its Repeater.
                                // The collapsed bucket must cost zero cards.
                                model: root.otherExpanded ? modelData.cards : []
                                delegate: AlbumCard {
                                    albumId: modelData.id
                                    title: modelData.title
                                    // year in the subtitle slot (card_to_item)
                                    artist: modelData.year
                                    artistId: ""
                                    genre: modelData.genre
                                    year: modelData.year
                                    qualityTier: modelData.qualityTier
                                    qualityDetail: modelData.qualityDetail || ""
                                    artSource: root.coverMap[modelData.artUrl] || ""
                                    isPinned: modelData.isPinned === true
                                    artworkUrl: modelData.artUrl || ""
                                    isFavorite: modelData.isFavorite === true
                                }
                            }
                        }
                    }
                }
            }

            // ================= In library tab =============================
            // The user's OWN rows for this artist (`root.libItems`, off the
            // artist document). Two first-class sections — Tracks modelled on
            // Popular Tracks (header action cluster, 10-row preview, Load
            // more) and the Albums grid — each carrying an `anchorId` so the
            // JUMP TO strip resolves against THIS column in library mode.
            Column {
                id: libraryTab
                visible: root.artistTab === "library" && !QbzArtist.artistLoading
                width: page.bodyWidth
                spacing: 0

                // Same bulk bar as the catalog column: `bulkAction` routes on
                // the active mode's lists (`selectableLists`).
                QbzMultiSelectBar {
                    visible: root.multiSelect
                    width: parent.width
                    selectedCount: root.selectedCount
                    actions: [
                        { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                        { "id": "play-next", "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "danger": false, "needsSelection": true },
                        { "id": "play-later", "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "danger": false, "needsSelection": true },
                        { "id": "queue", "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "danger": false, "needsSelection": true },
                        { "id": "add-to-playlist", "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "danger": false, "needsSelection": true },
                        { "id": "add-to-mixtape", "label": QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev), "icon": "cassette-tape", "danger": false, "needsSelection": true },
                        { "id": "make-offline", "label": QbzSession.tr("Make available offline", QbzSession.trRev), "icon": "cloud-download", "danger": false, "needsSelection": true },
                        { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true }
                    ]
                    onAction: function (id) { root.bulkAction(id) }
                }

                // --- Tracks ---------------------------------------------
                // Header + action cluster: the Popular Tracks row above,
                // with the actions re-pointed at the library list — Play all
                // / Shuffle all through `libraryPlayAll`, the ⋯ menu shared
                // with Popular Tracks (`topMenu.forLibrary`).
                Row {
                    property string anchorId: "tracks"
                    visible: root.libTracks.length > 0
                    width: parent.width
                    spacing: 12
                    Text {
                        width: parent.width - 44 - 32 - 32 - 3 * 12
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Tracks", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                    }
                    QbzCircleAction {
                        primary: true
                        name: "play-fill"
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzLibrary.libraryPlayAll(JSON.stringify(root.libTrackIds()), false)
                    }
                    QbzCircleAction {
                        name: "square-check-big"
                        active: root.multiSelect
                        btnEnabled: root.libTracks.length > 0
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: root.setMultiSelect(!root.multiSelect)
                    }
                    QbzCircleAction {
                        id: libMenuBtn
                        name: "ellipsis"
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: function (mouse) {
                            topMenu.forLibrary = true
                            topMenu.openAtCursor(libMenuBtn, mouse.x, mouse.y)
                        }
                    }
                }
                Item { visible: root.libTracks.length > 0; width: 1; height: 10 }
                Repeater {
                    // Numeric model: only the revealed rows are BUILT
                    // (declaring is building — the Popular Tracks expander's
                    // rule, feedback_qml_hidden_is_not_unbuilt).
                    model: root.libTracksExpanded ? root.libTracks.length
                        : Math.min(root.libTracks.length, root.libPreview)
                    delegate: PopularTrackRow {
                        readonly property var item: root.libTracks[index] || ({})
                        property bool revealed: false
                        // A cleared heart takes the row out — visually now,
                        // for real when the document republishes (the heart
                        // settles, `artist_qt::apply_favorite_change`
                        // recomputes the rows). A PURCHASE that loses its
                        // heart stays: it is still library (contract §3).
                        readonly property bool leaving:
                            !root.toggleState("track:" + item.id, item.isFavorite === true)
                            && item.group !== "purchases"
                        height: 50
                        opacity: (revealed && !leaving) ? 1.0 : 0.0
                        Behavior on opacity { NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }
                        Component.onCompleted: revealed = true
                        row: ({
                            "id": item.id, "number": index + 1, "title": item.title,
                            "artist": item.artist, "artistId": item.artistId,
                            "album": item.album, "albumId": item.albumId,
                            "duration": item.duration, "qualityTier": item.qualityTier,
                            "explicit": item.explicit, "artUrl": item.imageUrl,
                            "isFavorite": item.isFavorite === true,
                            "qobuzUnavailable": item.qobuzUnavailable === true,
                            "cacheStatus": item.cacheStatus || 0,
                        })
                        rowIndex: index
                        showAlbum: true
                        selectMode: root.multiSelect
                        checked: root.selected[row.id] === true
                        playHandler: function (id) {
                            QbzLibrary.libraryPlayVisible(JSON.stringify(root.libTrackIds()), id)
                        }
                        onToggleSelect: function (mods) { root.toggleSelected(row.id, mods) }
                    }
                }
                Item { visible: root.libTracks.length > root.libPreview; width: 1; height: 4 }
                QbzLoadMore {
                    visible: root.libTracks.length > root.libPreview
                    width: parent.width
                    label: root.libTracksExpanded ? QbzSession.tr("View less", QbzSession.trRev) : QbzSession.tr("Load more", QbzSession.trRev)
                    onClicked: root.libTracksExpanded = !root.libTracksExpanded
                }

                // --- Albums ---------------------------------------------
                Item { visible: root.libAlbums.length > 0 && root.libTracks.length > 0; width: 1; height: 24 }
                // Header: title, the release buckets' sort select, and the
                // split play button — the catalog section header's cluster
                // minus "See discography" (there is nothing to page here).
                Row {
                    property string anchorId: "albums"
                    visible: root.libAlbums.length > 0
                    width: parent.width
                    spacing: 12
                    Text {
                        width: parent.width - libSortBtn.width - libPlayBtn.width - 24
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Albums", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    QbzSelect {
                        id: libSortBtn
                        anchors.verticalCenter: parent.verticalCenter
                        sm: true
                        menuWidth: 150
                        options: [QbzSession.tr("Default", QbzSession.trRev),
                                  QbzSession.tr("Newest", QbzSession.trRev),
                                  QbzSession.tr("Oldest", QbzSession.trRev),
                                  QbzSession.tr("A–Z", QbzSession.trRev),
                                  QbzSession.tr("Z–A", QbzSession.trRev)]
                        currentIndex: {
                            var s = root.libSort || "default"
                            return s === "newest" ? 1 : s === "oldest" ? 2
                                 : s === "title-asc" ? 3 : s === "title-desc" ? 4 : 0
                        }
                        onSelected: function (i) {
                            root.setLibSort(i === 1 ? "newest" : i === 2 ? "oldest"
                                : i === 3 ? "title-asc" : i === 4 ? "title-desc" : "default")
                        }
                    }
                    QbzSplitButton {
                        id: libPlayBtn
                        anchors.verticalCenter: parent.verticalCenter
                        label: root.albumSectionLabel("library")
                        menuItems: root.albumSectionMenu("library")
                        onClicked: root.albumSectionPrimary("library", root.libAlbumsSorted)
                        onPicked: function (a) { root.albumSectionAction("library", root.libAlbumsSorted, a) }
                    }
                }
                Item { visible: root.libAlbums.length > 0; width: 1; height: 10 }
                Grid {
                    visible: root.libAlbums.length > 0
                    width: parent.width
                    columns: Math.max(1, Math.floor((width + 24) / 224))
                    columnSpacing: 24
                    rowSpacing: 24
                    Repeater {
                        model: root.libAlbumsSorted
                        delegate: AlbumCard {
                            // "Play selected" over this section (the split
                            // button), check bottom-right inside the art.
                            selectMode: root.albumSelectKey === "library"
                            selected: root.albumSelected[modelData.id] === true
                            selectMarkBottom: true
                            onSelectToggled: root.albumSelectToggle(modelData.id)
                            albumId: modelData.id
                            title: modelData.title
                            artist: modelData.artist
                            artistId: modelData.artistId
                            genre: modelData.genre
                            year: modelData.year
                            qualityTier: modelData.qualityTier
                            qualityDetail: modelData.qualityDetail || ""
                            artSource: root.coverMap[modelData.imageUrl] || ""
                            // These rows are LIBRARY feed items (FeedItem),
                            // not release cards: the remote url is
                            // `imageUrl`, and the pin state rides the same
                            // row (library_qt `map_album`).
                            isPinned: modelData.isPinned === true
                            artworkUrl: modelData.imageUrl || ""
                            // The heart goes through the page's optimistic
                            // store so the card can fade out on the click,
                            // ahead of the settle that removes the row.
                            hostFavorite: true
                            isFavorite: root.toggleState("album:" + modelData.id, modelData.isFavorite === true)
                            onFavoriteRequested: {
                                var next = !root.toggleState("album:" + modelData.id, modelData.isFavorite === true)
                                root.setToggleState("album:" + modelData.id, next)
                                QbzLibrary.libraryToggleFavorite("album", modelData.id)
                            }
                            readonly property bool leaving: !isFavorite && modelData.group !== "purchases"
                            opacity: leaving ? 0.0 : 1.0
                            Behavior on opacity { NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }
                        }
                    }
                }
            }
        }

    }

    // Gutter scrollbar. A SIBLING of the Flickable, not a child: anything
    // declared inside a Flickable lands in its contentItem and scrolls away
    // with the page. Hidden while the network panel is open — that panel pins
    // to the same right edge and carries its own scroll
    // (ArtistPageView.slint:1157).
    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: flick; scope: "artist" }
    QbzScrollBar {
        visible: !root.networkOpen
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: flick
    }

    // --- Network sidebar (300px, surface-card + 1px left border) ---------
    // BOUNDED, not full-height. ArtistPageView.slint:1178-1195:
    //   natural-top = the live viewport y of the body row + the JUMP TO bar
    //                 height  (i.e. the panel starts under the strip, never
    //                 beside the portrait)
    //   y           = max(natural-top, sticky-top=44)  — it rides the scroll
    //                 down and then parks 44px from the top
    //   height      = root.height - y                 — always flush with the
    //                 bottom, so no gap appears once it is parked
    // The port ran it top-to-bottom of the whole view, which put the panel
    // alongside the header and made it read as app chrome instead of a page
    // panel.
    // --- STICKY JUMP TO bar (ArtistPageView.slint:1113-1126) ---------------
    //
    // An OVERLAY over the Flickable, not a row inside it, and that is the only
    // way it can pin: an item in the content column scrolls with the content by
    // definition. `jumpSlot` above holds its place in the flow so nothing jumps
    // when it lifts out.
    //
    // The clamp is the reference's, verbatim: `y = max(naturalTop, 0)` where
    // naturalTop is the slot's VIEWPORT-relative y. The reference's own comment
    // is worth keeping in mind — the formula must NOT add the viewport y, or
    // the scroll is double-counted and the bar lifts ahead of the body during
    // the transition (a visible gap plus a bar-over-header artifact mid-scroll).
    //
    // Full width with padH 32, where the in-flow version was `parent.width - 64`
    // with padH 0: the Column supplied that 32px padding, and outside it the bar
    // has to supply its own or the strip would shift left by 32 when it sticks.
    // Net position of the tabs is unchanged. Same shape as the .slint, which
    // also mounts it at x:0 across `root.width`.
    QbzJumpNavBar {
        id: jumpBar
        x: 0
        width: root.width
        padH: 32
        y: Math.max(page.y + jumpSlot.y - flick.contentY, 0)
        // PINNED at the pane's top edge -> round the top corners, or the bar's
        // square ones poke out past the shell's rounded bezel (the Discover
        // full-bleed toolbar, same defect, same fix). Driven off the live y so
        // the rounding exists ONLY while the bar is actually at the top: in
        // mid-page it is surrounded by content and a notch there would be worse
        // than the square corner it fixes.
        topRadius: y <= 0 ? theme.radiusMd : 0
        // surface-main @ 0.6 under the dynamic background, NOT
        // transparent: pinned, it overlays the page content scrolling beneath
        // it, so it needs SOME fill to stay readable — the reference gives thin
        // bars their own lighter tier for exactly that
        // (ArtistPageView.slint:1108). This mattered less when the bar scrolled
        // with the page; now it is load-bearing.
        barBg: root.ambientOn
            ? Qt.rgba(theme.surfaceMain.r, theme.surfaceMain.g,
                      theme.surfaceMain.b, 0.60)
            : theme.surfaceMain
        tabs: root.jumpTabs
        activeTabId: root.activeJumpTab
        visible: root.jumpTabs.length > 0
        // "In library" source switches — `Library > Albums / Tracks`'s
        // purchases / favourites toggles (LibraryToolbar.qml:376-377), on the
        // shared QbzToggleButton, floating in the strip ONLY while the library
        // tab is active. State is the page's own (`libShowPurchases` /
        // `libShowFavorites`), never `QbzLibrary.sessionShow*`.
        trailing: Row {
            visible: root.artistTab === "library"
            width: visible ? implicitWidth : 0
            height: 44
            spacing: 8
            QbzToggleButton {
                sm: true
                name: "shopping-bag"
                active: root.libShowPurchases
                anchors.verticalCenter: parent.verticalCenter
                onClicked: root.libShowPurchases = !root.libShowPurchases
            }
            QbzToggleButton {
                sm: true
                name: "heart"
                active: root.libShowFavorites
                anchors.verticalCenter: parent.verticalCenter
                onClicked: root.libShowFavorites = !root.libShowFavorites
            }
        }
        onTabClicked: function (id) { root.scrollToSection(id) }
        onSearchEdited: function (text) { root.searchQuery = text }
    }

    Rectangle {
        id: netPanel
        // Viewport-relative y of the strip's bottom edge. `jumpBar.y` is
        // content coords, so subtracting the scroll offset gives the live
        // viewport position — the same arithmetic the .slint does with
        // absolute-position.
        readonly property real naturalTop:
            page.y + jumpSlot.y + jumpSlot.height - flick.contentY
        readonly property real stickyTop: 44

        anchors.right: parent.right
        y: Math.max(naturalTop, stickyTop)
        height: Math.max(0, root.height - y)
        width: root.networkOpen ? 300 : 0
        clip: true
        // Chrome tier: surface-card @ 0.5 under the dynamic background
        // (ArtistPageView.slint:1196). Its 44px header row stays TRANSPARENT
        // there (:1221) — it sits ON this already-translucent body, and a
        // second translucent layer would compound to near-opaque.
        color: root.ambientOn ? theme.surfaceCardA50 : theme.surfaceCard
        Behavior on width { NumberAnimation { duration: 160; easing.type: Easing.InOutQuad } }

        Rectangle { anchors.left: parent.left; anchors.top: parent.top; anchors.bottom: parent.bottom; width: 1; color: theme.borderSubtle }

        Column {
            anchors.fill: parent
            spacing: 0

            // Header: Network / Magazine tabs + close.
            Item {
                width: parent.width
                height: 44
                Row {
                    anchors.left: parent.left
                    anchors.leftMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 14
                    Repeater {
                        model: [
                            { "id": "network", "label": QbzSession.tr("Network", QbzSession.trRev) },
                            { "id": "magazine", "label": QbzSession.tr("Magazine", QbzSession.trRev) },
                        ]
                        delegate: Column {
                            required property var modelData
                            spacing: 0
                            Text {
                                text: modelData.label
                                color: root.netTab === modelData.id ? theme.textPrimary : theme.textMuted
                                font.pixelSize: 12
                                font.weight: theme.weightSemibold
                                font.letterSpacing: 0.8
                                MouseArea {
                                    anchors.fill: parent
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.netTab = modelData.id
                                }
                            }
                            Rectangle {
                                visible: root.netTab === modelData.id
                                width: parent.width
                                height: 2
                                radius: 1
                                color: theme.accent
                            }
                        }
                    }
                }
                Rectangle {
                    anchors.right: parent.right
                    anchors.rightMargin: 8
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
                    radius: 6
                    color: netCloseArea.containsMouse ? theme.surfaceElevated : "transparent"
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "panel-right-close"
                        width: 18
                        height: 18
                        tintName: netCloseArea.containsMouse ? root.tintOnSurface : "muted"
                    }
                    MouseArea {
                        id: netCloseArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.networkOpen = false
                    }
                }
            }
            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }

            // Network tab body.
            Flickable {
                visible: root.netTab === "network"
                width: parent.width
                height: parent.height - 45
                clip: true
                contentWidth: width
                contentHeight: netBody.implicitHeight
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: netBody
                    width: parent.width
                    topPadding: 4
                    bottomPadding: 12
                    spacing: 0

                    // ORIGIN (MusicBrainz). Gated exactly like the Slint
                    // block: MB available AND (still loading OR the artist
                    // actually carries a life span / location). With MB off
                    // the whole block is absent — nothing was requested.
                    Column {
                        visible: network.mbAvailable === true
                                 && (network.originLoading === true || mbOrigin.hasData === true)
                        width: parent.width
                        leftPadding: 14
                        rightPadding: 14
                        topPadding: 12
                        bottomPadding: 12
                        spacing: 6
                        SidebarSkeleton {
                            visible: root.originPending
                            rows: 2
                            phase: skeletonPhase.on
                        }
                        OriginRow {
                            visible: network.originLoading !== true && (mbOrigin.beginDate || "") !== ""
                            key: mbOrigin.isPerson ? QbzSession.tr("BORN", QbzSession.trRev)
                                                   : QbzSession.tr("FOUNDED", QbzSession.trRev)
                            value: mbOrigin.beginDate || ""
                        }
                        OriginRow {
                            visible: network.originLoading !== true && (mbOrigin.locationDisplay || "") !== ""
                            key: mbOrigin.isPerson ? QbzSession.tr("BORN IN", QbzSession.trRev)
                                                   : QbzSession.tr("FOUNDED IN", QbzSession.trRev)
                            // DOOR D1 to the Artist Scene, and the reference's
                            // ONLY door (ArtistDetailView.svelte:2904-2935).
                            // Was plain text under a POC-NOTE until the scene
                            // view existed to receive it.
                            value: mbOrigin.locationDisplay || ""
                            clickable: root.sceneAvailable
                            onActivated: root.openArtistScene()
                        }
                        OriginRow {
                            visible: network.originLoading !== true && (mbOrigin.endDate || "") !== ""
                            key: mbOrigin.isPerson ? QbzSession.tr("DIED", QbzSession.trRev)
                                                   : QbzSession.tr("DISBANDED", QbzSession.trRev)
                            value: mbOrigin.endDate || ""
                        }
                    }

                    // LABELS.
                    Column {
                        width: parent.width
                        leftPadding: 14
                        rightPadding: 14
                        topPadding: 12
                        bottomPadding: 6
                        spacing: 4
                        SidebarSectionHeading { text: QbzSession.tr("LABELS", QbzSession.trRev) }
                        SidebarNote {
                            visible: labels.length === 0
                            text: QbzSession.tr("No label info", QbzSession.trRev)
                        }
                        Repeater {
                            model: labels
                            delegate: SidebarLink {
                                label: modelData.name
                                tooltip: modelData.name
                                iconName: "disc"
                                onClicked: QbzHome.openLabel(modelData.id)
                            }
                        }
                    }
                    // SIMILAR ARTISTS.
                    Column {
                        visible: similarArtists.length > 0 || QbzArtist.artistLoading
                        width: parent.width
                        leftPadding: 14
                        rightPadding: 14
                        topPadding: 12
                        bottomPadding: 6
                        spacing: 4
                        SidebarSectionHeading { text: QbzSession.tr("SIMILAR ARTISTS", QbzSession.trRev) }
                        SidebarSkeleton {
                            visible: root.similarPending
                            rows: 4
                            phase: skeletonPhase.on
                        }
                        Repeater {
                            model: similarArtists
                            delegate: SidebarLink {
                                label: modelData.name
                                tooltip: modelData.name
                                iconName: "user"
                                onClicked: QbzArtist.openArtist(modelData.id)
                            }
                        }
                    }

                    // RELATIONSHIPS (MusicBrainz) — band members, the groups
                    // this artist belongs to, and studio collaborators.
                    Column {
                        visible: network.mbAvailable === true
                                 && (network.relationshipsLoading === true
                                     || mbRelationships.hasData === true)
                        width: parent.width
                        leftPadding: 14
                        rightPadding: 14
                        topPadding: 12
                        bottomPadding: 6
                        spacing: 6
                        SidebarSectionHeading { text: QbzSession.tr("RELATIONSHIPS", QbzSession.trRev) }
                        SidebarSkeleton {
                            visible: root.relationshipsPending
                            rows: 3
                            phase: skeletonPhase.on
                        }
                        RelationshipGroup {
                            visible: network.relationshipsLoading !== true
                                     && (mbRelationships.members || []).length > 0
                            title: QbzSession.tr("Members & Former", QbzSession.trRev)
                            rows: mbRelationships.members || []
                            roleKey: "member"
                            iconName: "user"
                            expanded: root.membersExpanded
                            onToggled: root.membersExpanded = !root.membersExpanded
                        }
                        RelationshipGroup {
                            visible: network.relationshipsLoading !== true
                                     && (mbRelationships.groups || []).length > 0
                            title: QbzSession.tr("Member Of", QbzSession.trRev)
                            rows: mbRelationships.groups || []
                            roleKey: "member of"
                            iconName: "music"
                            expanded: root.groupsExpanded
                            onToggled: root.groupsExpanded = !root.groupsExpanded
                        }
                        RelationshipGroup {
                            visible: network.relationshipsLoading !== true
                                     && (mbRelationships.collaborators || []).length > 0
                            title: QbzSession.tr("Collaborators", QbzSession.trRev)
                            rows: mbRelationships.collaborators || []
                            roleKey: "collaborator"
                            iconName: "user"
                            expanded: root.collabsExpanded
                            onToggled: root.collabsExpanded = !root.collabsExpanded
                        }
                    }

                    // YOU MAY ALSO LIKE (MusicBrainz tag discovery, validated
                    // against Qobuz by the core). Rows without a resolved
                    // Qobuz id stay informational instead of dead-clicking.
                    Column {
                        visible: network.mbAvailable === true
                                 && (network.discoveryLoading === true || root.discoveryRows.length > 0)
                        width: parent.width
                        leftPadding: 14
                        rightPadding: 14
                        topPadding: 12
                        bottomPadding: 12
                        spacing: 4
                        SidebarSectionHeading { text: QbzSession.tr("YOU MAY ALSO LIKE", QbzSession.trRev) }
                        SidebarSkeleton {
                            visible: root.discoveryPending
                            rows: 4
                            phase: skeletonPhase.on
                        }
                        Repeater {
                            model: root.discoveryRows
                            delegate: Item {
                                required property var modelData
                                // -28 = the section Column's left+right
                                // padding (see OriginRow).
                                width: parent ? parent.width - 28 : 0
                                height: 28
                                SidebarLink {
                                    anchors.left: parent.left
                                    anchors.verticalCenter: parent.verticalCenter
                                    // Explicit width REPLACES the component's
                                    // own `parent.width` binding (leaving it
                                    // and anchoring both edges fights it).
                                    width: parent.width - 26
                                    label: modelData.name
                                    tooltip: modelData.name
                                    iconName: "user"
                                    navigable: modelData.qobuzId !== ""
                                    onClicked: QbzArtist.openArtist(modelData.qobuzId)
                                }
                                Rectangle {
                                    id: dismissBtn
                                    anchors.right: parent.right
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: 24
                                    height: 24
                                    radius: 4
                                    color: dismissArea.containsMouse ? theme.surfaceElevated : "transparent"
                                    QbzIcon {
                                        anchors.centerIn: parent
                                        name: "thumbs-down"
                                        width: 12
                                        height: 12
                                        tintName: dismissArea.containsMouse ? root.tintOnSurface : "muted"
                                    }
                                    MouseArea {
                                        id: dismissArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        // Session-only: drop the row now. The
                                        // Slint app also persists it under the
                                        // discovery tag; that store is not open
                                        // in this port (see the handoff report).
                                        onClicked: {
                                            var d = root.dismissedDiscovery
                                            d[modelData.mbid] = true
                                            root.dismissedDiscovery = Object.assign({}, d)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Magazine tab body — Qobuz editorial story teasers (limit 2,
            // like the official client). A story opens in the system browser.
            Flickable {
                visible: root.netTab === "magazine"
                width: parent.width
                height: parent.height - 45
                clip: true
                contentWidth: width
                contentHeight: magBody.implicitHeight
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: magBody
                    width: parent.width
                    padding: 12
                    spacing: 10

                    // Story teasers are fetched after the page (Qobuz
                    // editorial): two card placeholders in the teaser shape
                    // while they resolve. Resolved-to-nothing keeps the
                    // explicit empty line below — this is a TAB body, where a
                    // blank panel would read as broken.
                    Column {
                        visible: root.storiesPending
                        width: magBody.width - 24
                        spacing: 12
                        Repeater {
                            model: 2
                            delegate: Column {
                                required property int index
                                width: parent ? parent.width : 0
                                spacing: 6
                                QbzSkeleton {
                                    variant: "block"
                                    width: parent.width
                                    height: parent.width
                                    blockRadius: 6
                                    cellIndex: index
                                    phase: skeletonPhase.on
                                }
                                QbzSkeleton { variant: "block"; width: parent.width * 0.82; height: 14; cellIndex: index; phase: skeletonPhase.on }
                                QbzSkeleton { variant: "block"; width: parent.width * 0.45; height: 11; cellIndex: index; phase: skeletonPhase.on }
                            }
                        }
                    }
                    SidebarNote {
                        visible: artist.storiesLoading !== true && stories.length === 0
                        text: QbzSession.tr("No stories for this artist", QbzSession.trRev)
                    }

                    Repeater {
                        model: stories
                        delegate: Rectangle {
                            required property var modelData
                            width: magBody.width - 24
                            height: storyCol.implicitHeight
                            radius: 8
                            color: storyArea.containsMouse ? theme.surfaceHover : "transparent"
                            Column {
                                id: storyCol
                                width: parent.width
                                padding: 6
                                spacing: 6
                                // 1:1 square thumbnail, height tracks width.
                                Rectangle {
                                    visible: (modelData.artUrl || "") !== ""
                                    width: storyCol.width - 12
                                    height: visible ? width : 0
                                    radius: 6
                                    color: theme.surfaceElevated
                                    clip: true
                                    RoundedImage {
                                        anchors.fill: parent
                                        source: root.coverMap[modelData.artUrl] || ""
                                        radius: 6
                                    }
                                }
                                Text {
                                    width: storyCol.width - 12
                                    text: modelData.title
                                    color: theme.textPrimary
                                    font.pixelSize: 13
                                    font.weight: theme.weightSemibold
                                    wrapMode: Text.WordWrap
                                }
                                Text {
                                    visible: (modelData.author || "") !== ""
                                    width: storyCol.width - 12
                                    text: modelData.author
                                    color: theme.textMuted
                                    font.pixelSize: 11
                                    elide: Text.ElideRight
                                }
                            }
                            MouseArea {
                                id: storyArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: if ((modelData.url || "") !== "") Qt.openUrlExternally(modelData.url)
                            }
                        }
                    }
                }
            }
        }
    }

    // --- Radio dropdown (the header disc) ----------------------------------
    // ArtistPageView.slint:440-460 — 180px, two rows, `close-on-click`. Left
    // aligned under the trigger (`x: 0px; y: 38px` there), which is what
    // openBelowLeft does; openBelowRight would hang a 180px panel off to the
    // LEFT of a 32px disc that sits at the start of the row.
    QbzContextMenu {
        id: radioMenu
        menuWidth: 180
        Repeater {
            model: [
                // Both labels already exist in the catalogue (they are the
                // TrackContextMenu's), so this adds no new msgid to the eight
                // locales.
                { "label": QbzSession.tr("QBZ Radio", QbzSession.trRev), "action": "qbz" },
                { "label": QbzSession.tr("Qobuz Radio", QbzSession.trRev), "action": "qobuz" },
            ]
            delegate: Rectangle {
                required property var modelData
                width: parent ? parent.width : 0
                height: 33
                radius: 5
                color: rmiArea.containsMouse ? theme.surfaceHover : "transparent"
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    spacing: 8
                    QbzIcon {
                        name: "radio"
                        width: 15
                        height: 15
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: rmiArea.containsMouse ? "textPrimary" : "secondary"
                    }
                    Text {
                        height: parent.height
                        width: parent.width - 23
                        text: modelData.label
                        color: rmiArea.containsMouse ? theme.textPrimary : theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                MouseArea {
                    id: rmiArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        radioMenu.close()
                        if (modelData.action === "qbz")
                            QbzHome.startArtistRadio(artist.id)
                        else
                            QbzHome.startQobuzArtistRadio(artist.id)
                    }
                }
            }
        }
    }

    // --- ⋯ overflow menu ---------------------------------------------------
    QbzContextMenu {
        id: overflowMenu
        menuWidth: 224
            Repeater {
                // `live: false` = no seam on this bridge. The .slint greys
                // its own unavailable rows the same way (Artist Scene is
                // `enabled: NetworkSidebarState.mb-available`,
                // ArtistPageView.slint:523) — dimmed, no hover, no click,
                // and the menu keeps its shape.
                model: [
                    { "label": QbzSession.tr("Create Artist Collection", QbzSession.trRev), "icon": "library-big", "action": "disco", "live": true },
                    // DOOR D2. This row rendered at opacity 0.4 with
                    // `action: "stub"` — the track's named defect class, a
                    // control that renders, persists and drives nothing. It is
                    // a Slint-era invention with no counterpart in the visual
                    // reference (kept on the owner's explicit instruction,
                    // contract ruling R4), and it shares D1's gate exactly so
                    // the two doors can never disagree about availability.
                    { "label": QbzSession.tr("Artist Scene", QbzSession.trRev), "icon": "map-pin", "action": "scene", "live": root.sceneAvailable },
                    { "label": QbzSession.tr("Share", QbzSession.trRev), "icon": "link", "action": "share", "live": true },
                    { "label": root.toggleState("artistPin", artist.isPinned) ? QbzSession.tr("Unpin", QbzSession.trRev) : QbzSession.tr("Pin", QbzSession.trRev), "icon": root.toggleState("artistPin", artist.isPinned) ? "pin-filled" : "pin", "action": "pin", "live": true },
                ]
                delegate: Rectangle {
                    required property var modelData
                    width: parent ? parent.width : 0
                    height: 33
                    radius: 5
                    opacity: modelData.live ? 1.0 : 0.4
                    color: (modelData.live && omiArea.containsMouse) ? theme.surfaceHover : "transparent"
                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: 8
                        spacing: 8
                        QbzIcon { name: modelData.icon; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                        Text {
                            height: parent.height
                            width: parent.width - 23
                            text: modelData.label
                            color: theme.textSecondary
                            font.pixelSize: 13
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                    }
                    MouseArea {
                        id: omiArea
                        anchors.fill: parent
                        enabled: modelData.live
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            overflowMenu.close()
                            if (modelData.action === "pin") {
                                root.setToggleState("artistPin", !root.toggleState("artistPin", artist.isPinned))
                                QbzLibrary.togglePin("artist", artist.id, artist.name, "", artist.artUrl)
                            } else if (modelData.action === "scene") {
                                root.openArtistScene()
                            } else if (modelData.action === "disco") {
                                // Discography Builder — ArtistPageView.slint
                                // :505-511 `media-action("artist", id,
                                // "build-collection")`. This is the ONLY route
                                // to it; the nav flyout has no builder entry.
                                QbzDisco.open(artist.id)
                            } else if (modelData.action === "share") {
                                // ArtistPageView.slint:530-538 -> main.rs
                                // :12749 `media-action("artist", id,
                                // "share")`. The bridge builds the
                                // play.qobuz.com/artist/{id} URL, copies it
                                // and raises the "Link copied" toast, exactly
                                // as the .slint arm does. Nothing is copied
                                // QML-side here (unlike TrackRow.qml's
                                // Loader/TextEdit idiom): cxx-qt-lib exposes
                                // no QClipboard, and only Rust can publish
                                // QbzShell.toastJson, so the copy and its
                                // confirmation stay on one side.
                                //
                                // Link-only — artists have no Song.link path
                                // (share.rs's Song.link resolvers serve the
                                // track and album menus).
                                QbzArtist.share(artist.id)
                            }
                        }
                    }
                }
            }
            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
            // ArtistPageView.slint:557-572 — the 1px border-subtle separator
            // above, then the LAST item, whose label flips
            // "Show artist" / "Blacklist artist" on ArtistState.is-blacklisted.
            // LIVE since QbzBlacklist landed (`artistToggle(id, name)`); it was
            // dimmed-and-inert only while the bridge had no invokable.
            Rectangle {
                width: parent.width
                height: 33
                radius: 5
                color: blkArea.containsMouse ? theme.surfaceHover : "transparent"
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    spacing: 8
                    QbzIcon { name: "blind-eye"; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                    Text {
                        height: parent.height
                        width: parent.width - 23
                        text: root.artistBlacklisted
                            ? QbzSession.tr("Show artist", QbzSession.trRev)
                            : QbzSession.tr("Blacklist artist", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                MouseArea {
                    id: blkArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        overflowMenu.close()
                        root.toggleBlacklist()
                    }
                }
            }
        }

    // --- Popular Tracks ⋯ menu ---------------------------------------------
    QbzContextMenu {
        id: topMenu
        menuWidth: 224
        /// Opened from the library tab's Tracks header: the five rows act on
        /// `root.libTrackIds()` through the library seams instead of the
        /// TOP_QUEUE ones. Set by whichever ⋯ button opens the menu.
        property bool forLibrary: false
            Repeater {
                model: [
                    { "label": QbzSession.tr("Play all next", QbzSession.trRev), "icon": "list-start", "action": "next-all" },
                    { "label": QbzSession.tr("Play all later", QbzSession.trRev), "icon": "list-plus", "action": "later-all" },
                    { "label": QbzSession.tr("Add all to queue", QbzSession.trRev), "icon": "list-end", "action": "queue-all" },
                    { "label": QbzSession.tr("Shuffle all", QbzSession.trRev), "icon": "shuffle", "action": "shuffle-all" },
                    { "label": QbzSession.tr("Add all to playlist", QbzSession.trRev), "icon": "list-music", "action": "playlist-all" },
                ]
                delegate: Rectangle {
                    required property var modelData
                    width: parent ? parent.width : 0
                    height: 33
                    radius: 5
                    color: tmiArea.containsMouse ? theme.surfaceHover : "transparent"
                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: 8
                        spacing: 8
                        QbzIcon { name: modelData.icon; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                        Text {
                            height: parent.height
                            width: parent.width - 23
                            text: modelData.label
                            color: theme.textSecondary
                            font.pixelSize: 13
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                    }
                    MouseArea {
                        id: tmiArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            topMenu.close()
                            var a = modelData.action
                            // Library arm: the same five rows over the
                            // library tab's list — shuffle through the
                            // library play-all seam, the three enqueues
                            // through the bulk funnel with the whole list,
                            // and the picker over that list (the
                            // `playlist-all` branch below reads the mode).
                            if (topMenu.forLibrary && a !== "playlist-all") {
                                var lib = root.libTrackIds()
                                if (lib.length === 0) return
                                if (a === "shuffle-all") QbzLibrary.libraryPlayAll(JSON.stringify(lib), true)
                                else if (a === "next-all") QbzPlayer.bulkTracksAction(JSON.stringify(lib), "play-next", "artist", String(artist.id || ""))
                                else if (a === "later-all") QbzPlayer.bulkTracksAction(JSON.stringify(lib), "play-later", "artist", String(artist.id || ""))
                                else if (a === "queue-all") QbzPlayer.bulkTracksAction(JSON.stringify(lib), "queue", "artist", String(artist.id || ""))
                                return
                            }
                            if (a === "shuffle-all") QbzPlayer.playArtistTop(true)
                            // "Play all next" INSERTS at the cursor; it does
                            // not replay the artist. It used to call
                            // playArtistTop(false) — "Play all" — so the row
                            // labelled "next" wiped the queue and started
                            // over (ArtistPageView's twin in Slint enqueues,
                            // main.rs:15301).
                            else if (a === "next-all") QbzPlayer.enqueueArtistTop("next")
                            // #442 block tail — enqueue_track_list_mode's
                            // "later" arm, same funnel next-all/queue-all use.
                            else if (a === "later-all") QbzPlayer.enqueueArtistTop("later")
                            else if (a === "queue-all") QbzPlayer.enqueueArtistTop("queue")
                            else if (a === "playlist-all") {
                                // ArtistPageView.slint:797-802
                                // `top-tracks-menu-action("playlist-all")` —
                                // the picker over the section's tracks. `root
                                // .topTracks` is the FILTERED list, which is
                                // what `ArtistState.top-tracks` holds too
                                // (artist.rs filters into the state, the view
                                // never sees the raw list). Every row here is
                                // a Qobuz catalog track (`/artist/page`
                                // top_tracks), so the catalog arm is correct.
                                var ids = []
                                if (topMenu.forLibrary) ids = root.libTrackIds()
                                else for (var i = 0; i < root.topTracks.length; i++)
                                    ids.push(String(root.topTracks[i].id))
                                if (ids.length > 0)
                                    QbzPlaylistPicker.openForTracks(JSON.stringify(ids))
                            }
                        }
                    }
                }
            }
        }

    // --- Artist portrait: menu + lightbox -----------------------------------
    // View-root siblings, never descendants of the portrait frame (the frame
    // is a 200px circle; anything inside it would be masked away).

    /// Best available portrait source for the lightbox: the custom override
    /// first, then the file the artwork pipeline ALREADY downloaded for this
    /// page, then the remote URL as a last resort.
    ///
    /// Deliberate divergence from AlbumView.bestCoverSource(), which goes
    /// straight to the remote URL and re-downloads a file it already has on
    /// disk. Preferring the cache also matters more here: the artist `large`
    /// segment is the ORIGINAL upload (measured on this machine's cache:
    /// n=149, median 1439px, max 5679px), so the fetch it avoids is a big one.
    function bestArtistImage() {
        var custom = artist.customImageUrl || ""
        if (custom !== "") return custom
        var cached = root.coverMap[artist.artUrl] || ""
        if (cached !== "") return cached
        return artist.artUrl || ""
    }

    /// The portrait menu's rows, rebuilt per open so Add/Change/Remove track
    /// the live flag (ArtistPageView.slint:307-351, plus "View image" — the
    /// lightbox entry, this port's addition and the twin of the album menu's
    /// "View cover").
    ///
    /// The two url-only rows are hidden when there is nothing for them to act
    /// on. The reference passes `ArtistState.artwork-url` blindly and shows
    /// them even for an artist Qobuz has no portrait for, where they do
    /// nothing — a small, deliberate divergence rather than a copied hole.
    /// "Save as…" survives an empty url when a custom image exists, because
    /// that path saves the local file.
    function buildPortraitMenuModel() {
        var rows = []
        if (artist.hasCustomImage === true) {
            rows.push({ "label": QbzSession.tr("Change image", QbzSession.trRev), "icon": "image-plus", "action": "add" })
            rows.push({ "label": QbzSession.tr("Remove image", QbzSession.trRev), "icon": "trash-2", "action": "remove" })
        } else {
            rows.push({ "label": QbzSession.tr("Add image", QbzSession.trRev), "icon": "image-plus", "action": "add" })
        }
        if (root.bestArtistImage() !== "")
            rows.push({ "label": QbzSession.tr("View image", QbzSession.trRev), "icon": "eye", "action": "view" })
        if ((artist.artUrl || "") !== "")
            rows.push({ "label": QbzSession.tr("Open in browser", QbzSession.trRev), "icon": "external-link", "action": "browser" })
        if ((artist.artUrl || "") !== "" || artist.hasCustomImage === true)
            rows.push({ "label": QbzSession.tr("Save as…", QbzSession.trRev), "icon": "cloud-download", "action": "save" })
        return rows
    }

    QbzContextMenu {
        id: portraitMenu
        menuWidth: 196
        onAboutToShow: portraitMenuRepeater.model = root.buildPortraitMenuModel()
        Repeater {
            id: portraitMenuRepeater
            model: []
            delegate: Rectangle {
                required property var modelData
                width: parent ? parent.width : 0
                height: 33
                radius: 5
                color: pmiArea.containsMouse ? theme.surfaceHover : "transparent"
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    spacing: 8
                    QbzIcon { name: modelData.icon; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                    Text {
                        height: parent.height
                        width: parent.width - 23
                        text: modelData.label
                        color: theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                MouseArea {
                    id: pmiArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        portraitMenu.close()
                        var a = modelData.action
                        // The store is keyed by NAME (ArtistPageView.slint:312),
                        // so the name — not the id — is what travels.
                        if (a === "add") QbzArtist.imageAddCustom(artist.name || "", artist.artUrl || "")
                        else if (a === "remove") QbzArtist.imageRemoveCustom(artist.name || "", artist.artUrl || "")
                        else if (a === "view") portraitLightbox.openWith(root.bestArtistImage())
                        else if (a === "browser") QbzShell.openExternalUrl(artist.artUrl)
                        else if (a === "save") QbzArtist.imageSaveAs(artist.name || "", artist.artUrl || "")
                    }
                }
            }
        }
    }

    CoverLightbox { id: portraitLightbox }
}
