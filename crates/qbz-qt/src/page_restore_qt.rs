//! Where the user left off — THE PAGE, not just its section (2026-09-13).
//!
//! `nav_qt`'s `last_view` pref remembers the last ROOT view, in the vocabulary
//! the Slint build shares through `ui_prefs.json`, so "Startup page = Where
//! you left off" could only ever reopen on Home, Library, Local Library,
//! Mixtapes or Collections: a session closed on an album came back on
//! Library > Releases. The rule now is the page the user was on — with its
//! arguments (the album, the artist, the playlist, the search query, the
//! Settings section…) and the tab it was showing.
//!
//! Two layers, both written as the user moves:
//!   - `last_view` (`nav_qt::record_entry`) keeps its meaning and its shared
//!     vocabulary: the last root, which is also the history seeded UNDER a
//!     restored page (so Back has somewhere to go) and the fallback when the
//!     page cannot come back.
//!   - this module's `last_page_qt.json`, per profile beside
//!     `local_navigation_qt.json`: `{"page": {root, view, args, state}}`.
//!     `view` and `args` come from `nav_qt::record_with` — every restorable
//!     opener records its own arguments — or from `note_args` for the pages
//!     whose arguments live outside their route push (the Settings section,
//!     the search query, the purchased album); `state` is the tab the mounted
//!     view reported through `QbzShell.reportNavState`.
//!
//! The restore is ONE-SHOT per process and honours the gates the local album
//! restore honours: the pref must say "remember", a crash chain bypasses it
//! (nav_qt's ladder), an explicit launcher link outranks it, and the kiosk
//! keeps its own navigation model. The shell mounts the page's view at
//! construction (no Home flash), the history is seeded with the page's root,
//! and `restore()` re-runs the opener at session entry. A page that needs the
//! catalog is skipped while the session is offline, Purchases stays behind its
//! opt-in, and a page that cannot come back lands on the root instead.
//!
//! Local albums stay with `local_restore_qt`, which owns their filter context:
//! this module never stores `localalbum`. Leaving the app on a page that only
//! its opener could rebuild (`scene`, `musician`, `discobuilder`,
//! `metadataeditor`) keeps the previous restorable page in the document, so
//! the restart lands one step back rather than on Home.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

const FILE: &str = "last_page_qt.json";
const MAX_ARGS_BYTES: usize = 8 * 1024;
const MAX_STATE_BYTES: usize = 64 * 1024;

/// Routes whose page can be rebuilt from what is stored. Everything else in
/// nav_qt's id list is left out on purpose (see the module header).
const RESTORABLE: &[&str] = &[
    "home",
    "library",
    "local",
    "mixtapes",
    "collections",
    "album",
    "artist",
    "playlist",
    "label",
    "labelreleases",
    "mixtapedetail",
    "award",
    "awardalbums",
    "mix",
    "artistreleases",
    "discoverbrowse",
    "playlistbrowse",
    "recentalbums",
    "mostplayedalbums",
    "purchases",
    "purchase-album",
    "settings",
    "search",
    "queue-view",
    "blacklist",
    "playlistmanager",
    "offlinemanager",
    "libraryfolders",
];

/// Pages that cannot show anything without the catalog: skipped while the
/// session is offline (a Qobuz playlist joins them at restore time; a local
/// one does not).
const NEEDS_CATALOG: &[&str] = &[
    "album",
    "artist",
    "label",
    "labelreleases",
    "award",
    "awardalbums",
    "mix",
    "artistreleases",
    "discoverbrowse",
    "playlistbrowse",
    "purchases",
    "purchase-album",
    "search",
];

/// The roots nav_qt persists as `last_view` (its `VIEW_TO_PREF` set).
const ROOTS: &[&str] = &["home", "library", "local", "mixtapes", "collections"];

/// The persisted page.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Page {
    /// The last root view: the history seed and the fallback.
    #[serde(default)]
    pub root: String,
    /// The route id the user was on.
    #[serde(default)]
    pub view: String,
    /// The opener's arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
    /// The tab the mounted view reported, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Map<String, Value>>,
}

impl Page {
    fn valid(&self) -> bool {
        RESTORABLE.contains(&self.view.as_str())
            && serde_json::to_string(&self.args).is_ok_and(|s| s.len() <= MAX_ARGS_BYTES)
            && self
                .state
                .as_ref()
                .is_none_or(|s| serde_json::to_string(s).is_ok_and(|t| t.len() <= MAX_STATE_BYTES))
    }

    /// The view the shell records at session entry. The two listings that
    /// read their parent's state come back THROUGH that parent — the entry
    /// records it, `restore` opens the parent and then the listing, and the
    /// history reads root -> parent -> listing, the way it was reached.
    pub fn entry_view(&self) -> &str {
        match self.view.as_str() {
            "labelreleases" => "label",
            "awardalbums" => "award",
            other => other,
        }
    }

    fn arg(&self, key: &str) -> String {
        self.args
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    fn arg_i64(&self, key: &str) -> i64 {
        self.args.get(key).and_then(Value::as_i64).unwrap_or(0)
    }

    fn tab(&self) -> Option<String> {
        self.state
            .as_ref()
            .and_then(|s| s.get("activeTab"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    }
}

// ---------------------------------------------------------------------------
//  The live document (pure — the globals below wrap it)
// ---------------------------------------------------------------------------

/// What the process currently believes the page is, plus arguments noted
/// for a route that has not been recorded yet (`note_args` before the QML
/// side pushes the route — the purchased album).
#[derive(Default)]
struct Live {
    page: Option<Page>,
    pending: Option<(String, Map<String, Value>)>,
}

impl Live {
    /// A route was recorded (`nav_qt::record_entry` / `step`). Returns the
    /// page to persist when something changed.
    fn note_page(&mut self, view: &str, args: &Value) -> Option<Page> {
        let page = self.page.get_or_insert_with(Page::default);
        let before = page.clone();
        if ROOTS.contains(&view) {
            page.root = view.to_string();
        }
        if RESTORABLE.contains(&view) {
            let pending = self
                .pending
                .take()
                .filter(|(pending_view, _)| pending_view == view)
                .map(|(_, args)| args);
            let explicit = match args {
                Value::Object(map) if !map.is_empty() => Some(map.clone()),
                _ => None,
            };
            page.view = view.to_string();
            page.args = explicit.or(pending).unwrap_or_default();
            page.state = None;
        }
        (*page != before).then(|| page.clone())
    }

    /// Arguments for a route: applied in place when it is the current page,
    /// otherwise kept for its next record.
    fn note_args(&mut self, view: &str, args: Map<String, Value>) -> Option<Page> {
        if serde_json::to_string(&args).is_ok_and(|s| s.len() > MAX_ARGS_BYTES) {
            return None;
        }
        if let Some(page) = self.page.as_mut() {
            if page.view == view {
                if page.args == args {
                    return None;
                }
                page.args = args;
                return Some(page.clone());
            }
        }
        self.pending = Some((view.to_string(), args));
        None
    }

    /// The mounted view reported its state: only the tab is restart
    /// material (filters and selections are history material, and Local
    /// Library persists its own richer set).
    fn note_state(&mut self, scope: &str, state: &str) -> Option<Page> {
        if state.len() > MAX_STATE_BYTES {
            return None;
        }
        let page = self.page.as_mut()?;
        if page.view != scope {
            return None;
        }
        let tab = serde_json::from_str::<Value>(state)
            .ok()
            .and_then(|v| v.get("activeTab").cloned())
            .filter(|t| t.as_str().is_some_and(|s| !s.is_empty()));
        let next = tab.map(|t| Map::from_iter([("activeTab".to_string(), t)]));
        if page.state == next {
            return None;
        }
        page.state = next;
        Some(page.clone())
    }
}

static LIVE: Mutex<Live> = Mutex::new(Live {
    page: None,
    pending: None,
});
static WRITE: Mutex<()> = Mutex::new(());
static STARTUP: OnceLock<Option<Page>> = OnceLock::new();
static TAKEN: AtomicBool = AtomicBool::new(false);

fn path() -> Option<PathBuf> {
    use qbz_app::user_data::UserDataPaths;
    // nav_qt's tests push routes through the real `record`: they must never
    // write into the developer's profile.
    if cfg!(test) {
        return None;
    }
    // Same identity as local_restore_qt, including the guest profile.
    let user = UserDataPaths::load_last_user_id().unwrap_or(0);
    Some(UserDataPaths::data_dir_for(user).ok()?.join(FILE))
}

fn save_at(path: &Path, page: &Page) {
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut doc) = crate::settings_qt::read_json_object(path) else {
        return;
    };
    let next = json!(page);
    if doc.get("page") != Some(&next) {
        doc.insert("page".into(), next);
        crate::settings_qt::write_json_object_atomic(path, &doc);
    }
}

fn load_at(path: &Path) -> Option<Page> {
    let doc = crate::settings_qt::read_json_object(path)?;
    let page: Page = serde_json::from_value(doc.get("page")?.clone()).ok()?;
    page.valid().then_some(page)
}

fn persist(page: Option<Page>) {
    if let (Some(page), Some(path)) = (page, path()) {
        save_at(&path, &page);
    }
}

fn live<R>(f: impl FnOnce(&mut Live) -> R) -> R {
    f(&mut LIVE.lock().unwrap_or_else(|e| e.into_inner()))
}

// ---------------------------------------------------------------------------
//  Writers (nav_qt, the bridges)
// ---------------------------------------------------------------------------

/// A route reached the top of the history (record) or the cursor moved onto
/// it (back/forward). `args` is the opener's argument object, or Null.
pub fn note_page(view: &str, args: &Value) {
    if crate::kiosk_profile_qt::active() {
        return;
    }
    persist(live(|l| l.note_page(view, args)));
}

/// Arguments for a route that does not carry them on its route push.
pub fn note_args(view: &str, args: Value) {
    if crate::kiosk_profile_qt::active() {
        return;
    }
    let Value::Object(args) = args else { return };
    persist(live(|l| l.note_args(view, args)));
}

/// The mounted view's live state (`nav_qt::set_live_state`).
pub fn note_state(scope: &str, state: &str) {
    if crate::kiosk_profile_qt::active() {
        return;
    }
    persist(live(|l| l.note_state(scope, state)));
}

// ---------------------------------------------------------------------------
//  Startup
// ---------------------------------------------------------------------------

fn startup_page_at(
    path: &Path,
    remember: bool,
    crash_level: u8,
    link: bool,
    kiosk: bool,
) -> Option<Page> {
    if !remember || crash_level >= 2 || link || kiosk {
        return None;
    }
    load_at(path)
}

/// The page this process opens on, decided ONCE (the shell bridge asks at
/// construction, the history seed and the session entry ask later, and all
/// three must agree). `None` = the ordinary `last_view` startup.
fn startup_page() -> Option<&'static Page> {
    STARTUP
        .get_or_init(|| {
            let path = path()?;
            let page = startup_page_at(
                &path,
                crate::settings_qt::pref_str("startup_page", "home") == "remember",
                crate::nav_qt::crash_level(),
                crate::deep_link_qt::has_pending(),
                crate::kiosk_profile_qt::active(),
            )?;
            log::info!(
                "[qbz-qt] startup: remember -> page {:?} args {} tab {:?} (root {:?})",
                page.view,
                Value::Object(page.args.clone()),
                page.tab(),
                page.root
            );
            Some(page)
        })
        .as_ref()
}

/// The view the shell mounts at construction and records at session entry.
pub fn startup_view() -> Option<String> {
    startup_page().map(|p| p.entry_view().to_string())
}

/// The root seeded beneath the restored page, so Back leads somewhere.
pub fn startup_root() -> Option<String> {
    startup_page()
        .map(|p| p.root.clone())
        .filter(|root| ROOTS.contains(&root.as_str()))
}

/// Only the first session entry of this process restores; a launcher link
/// that arrived after the shell was built still outranks the page.
pub fn take_startup_page() -> Option<Page> {
    if TAKEN.swap(true, Ordering::AcqRel) {
        return None;
    }
    if crate::deep_link_qt::has_pending() || crate::kiosk_profile_qt::active() {
        return None;
    }
    startup_page().cloned()
}

/// Re-run the page's opener at session entry. The entry view is already
/// recorded and hydrated by the caller; a detail's opener records the same
/// route again (a no-op push) and loads its document. `false` = the page
/// cannot come back and the caller lands on the root instead.
pub fn restore(page: &Page) -> bool {
    let status = crate::offline_fwd::engine().status();
    let offline = status.is_offline();
    let id = page.arg("id");
    let needs_catalog = NEEDS_CATALOG.contains(&page.view.as_str())
        || (page.view == "playlist" && !crate::local_playlist_qt::is_local_id(&id));
    if offline && needs_catalog {
        log::info!(
            "[qbz-qt] startup: page {:?} needs the catalog and the session is offline; landing on the root",
            page.view
        );
        return false;
    }
    let with_id = |open: &dyn Fn(String)| -> bool {
        if id.is_empty() {
            return false;
        }
        open(id.clone());
        true
    };
    match page.view.as_str() {
        "home" | "library" => {
            if let Some(tab) = page.tab() {
                crate::navigate_to_tab(&page.view, &tab);
            }
            true
        }
        // Local Library restores its own tab (local_restore_qt); the MyQBZ
        // grids were hydrated with the entry.
        "local" | "mixtapes" | "collections" => true,
        "album" => with_id(&crate::open_album),
        "artist" => with_id(&crate::open_artist),
        "playlist" => with_id(&crate::open_playlist),
        "label" => with_id(&crate::label_qt::open_label),
        "labelreleases" => {
            if !with_id(&crate::label_qt::open_label) {
                return false;
            }
            crate::label_qt::open_releases();
            true
        }
        "mixtapedetail" => with_id(&crate::myqbz_detail_qt::open),
        "award" => {
            let name = page.arg("name");
            with_id(&|id| crate::award_qt::open_award(id, name.clone()))
        }
        "awardalbums" => {
            let name = page.arg("name");
            if !with_id(&|id| crate::award_qt::open_award(id, name.clone())) {
                return false;
            }
            crate::award_qt::open_albums();
            true
        }
        "mix" => {
            let kind = page.arg("kind");
            if kind.is_empty() {
                return false;
            }
            crate::foryou_qt::open_mix(kind);
            true
        }
        "artistreleases" => {
            let artist_id = page.arg("artistId");
            if artist_id.is_empty() {
                return false;
            }
            crate::artist_releases_qt::open(
                artist_id,
                page.arg("artistName"),
                page.arg("releaseType"),
            );
            true
        }
        "discoverbrowse" => {
            let endpoint = page.arg("endpoint");
            if endpoint.is_empty() {
                return false;
            }
            crate::browse_qt::open_discover_browse(endpoint, page.arg("title"));
            true
        }
        "playlistbrowse" => {
            crate::browse_qt::open_playlist_browse();
            true
        }
        "recentalbums" => {
            crate::browse_qt::open_recent_albums();
            true
        }
        "mostplayedalbums" => {
            crate::browse_qt::open_most_played_albums();
            true
        }
        // Opt-in surface: never reopen the app on a page whose own entry
        // point is switched off. The list loads itself on mount.
        "purchases" => crate::settings_qt::pref_bool("show_purchases", false),
        "purchase-album" => {
            if !crate::settings_qt::pref_bool("show_purchases", false) {
                return false;
            }
            with_id(&crate::purchases_qt::open_album)
        }
        "settings" => {
            // Same guard as the bridge's `settings_set_section`.
            let section = page.arg_i64("section").clamp(0, 64) as i32;
            let section = if section == 11 && !crate::orbit_qt::enabled() {
                0
            } else {
                section
            };
            crate::ui(move |mut b| b.as_mut().set_settings_section(section));
            true
        }
        "search" => {
            let query = page.arg("query");
            if query.chars().count() < 2 {
                return false;
            }
            let tab = page.arg_i64("tab").clamp(0, 16) as i32;
            let runtime = crate::app();
            crate::spawn(async move {
                crate::search_qt::submit(&runtime, &query, Some(tab)).await;
            });
            true
        }
        // Mounted by the entry; reads live bridges or refreshes itself.
        "queue-view" | "libraryfolders" => true,
        "blacklist" => {
            crate::blacklist_qt::open_manager();
            true
        }
        "playlistmanager" => {
            crate::playlist_manager_qt::navigate();
            true
        }
        "offlinemanager" => {
            crate::offline_manager_qt::open();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(view: &str) -> Page {
        Page {
            root: "library".into(),
            view: view.into(),
            args: Map::new(),
            state: None,
        }
    }

    #[test]
    fn a_detail_keeps_its_arguments_and_a_root_becomes_the_fallback() {
        let mut live = Live::default();
        assert!(live.note_page("library", &Value::Null).is_some());
        let album = live
            .note_page("album", &json!({"id": "123"}))
            .expect("the page changed");
        assert_eq!(album.view, "album");
        assert_eq!(album.root, "library");
        assert_eq!(album.args.get("id").and_then(Value::as_str), Some("123"));
        assert_eq!(album.entry_view(), "album");
        // The same push again changes nothing.
        assert!(live.note_page("album", &json!({"id": "123"})).is_none());
        // A page only its opener could rebuild keeps the album on record.
        assert!(live.note_page("scene", &Value::Null).is_none());
        assert_eq!(live.page.as_ref().unwrap().view, "album");
        // Back onto Home: root and view move together.
        let home = live.note_page("home", &Value::Null).unwrap();
        assert_eq!((home.root.as_str(), home.view.as_str()), ("home", "home"));
        assert!(home.args.is_empty());
    }

    #[test]
    fn arguments_noted_before_the_route_push_ride_that_push() {
        let mut live = Live::default();
        live.note_page("purchases", &Value::Null);
        assert!(live
            .note_args(
                "purchase-album",
                json!({"id": "9"}).as_object().unwrap().clone()
            )
            .is_none());
        let detail = live.note_page("purchase-album", &Value::Null).unwrap();
        assert_eq!(detail.args.get("id").and_then(Value::as_str), Some("9"));
        // On the current page they apply in place; unchanged ones are quiet.
        live.note_page("settings", &Value::Null);
        let settings = live
            .note_args(
                "settings",
                json!({"section": 4}).as_object().unwrap().clone(),
            )
            .unwrap();
        assert_eq!(settings.arg_i64("section"), 4);
        assert!(live
            .note_args(
                "settings",
                json!({"section": 4}).as_object().unwrap().clone()
            )
            .is_none());
    }

    #[test]
    fn only_the_tab_of_the_current_page_is_kept_from_a_state_report() {
        let mut live = Live::default();
        live.note_page("library", &Value::Null);
        let tabbed = live
            .note_state("library", r#"{"activeTab":"albums","filter":"x"}"#)
            .unwrap();
        assert_eq!(tabbed.tab().as_deref(), Some("albums"));
        assert!(tabbed.state.as_ref().unwrap().get("filter").is_none());
        assert!(live
            .note_state("home", r#"{"activeTab":"forYou"}"#)
            .is_none());
        assert!(live
            .note_state("library", "not json")
            .is_some_and(|p| p.state.is_none()));
        // A new page starts without a tab.
        let album = live.note_page("album", &json!({"id": "1"})).unwrap();
        assert!(album.state.is_none());
    }

    #[test]
    fn the_document_round_trips_and_the_gates_bypass_it_without_deleting_it() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("users/1/last_page_qt.json");
        let mut saved = page("labelreleases");
        saved.args.insert("id".into(), json!("77"));
        save_at(&file, &saved);
        assert_eq!(
            startup_page_at(&file, true, 1, false, false),
            Some(saved.clone())
        );
        assert_eq!(saved.entry_view(), "label");
        for (remember, crash, link, kiosk) in [
            (false, 1, false, false),
            (true, 2, false, false),
            (true, 1, true, false),
            (true, 1, false, true),
        ] {
            assert!(startup_page_at(&file, remember, crash, link, kiosk).is_none());
        }
        assert_eq!(startup_page_at(&file, true, 1, false, false), Some(saved));
        // An unrestorable or malformed page is ignored, never crashed on.
        for bad in [json!({"view": "scene"}), json!({"view": 7}), json!("album")] {
            std::fs::write(&file, json!({"page": bad}).to_string()).unwrap();
            assert!(startup_page_at(&file, true, 1, false, false).is_none());
        }
    }
}
