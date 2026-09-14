//! Where OUR window sits on a Plasma Wayland desktop (2026-09-14).
//!
//! The "Wallpaper" background (wallpaper_qt.rs, mode 3) paints the part of
//! the desktop picture that lies UNDER the window, so moving the window slides
//! the picture the way a translucent window would. That needs the window's
//! place on the screen, and Wayland hides it from clients by design: Qt's
//! `QWindow::position()` is a fiction there. KDE Plasma does tell a client
//! where every window is — through `org_kde_plasma_window_management`, the
//! protocol its own task manager and window switchers use — and this module
//! reads it back for the windows of this process.
//!
//! WHY A SEPARATE CONNECTION. Qt owns the process's Wayland display and reads
//! it from its own event loop; sharing that `wl_display` would mean a foreign
//! event queue dispatched from a thread Qt does not know about. A second
//! client connection is ordinary Wayland (any process may hold several), costs
//! one socket, and lives on its own thread that blocks in `dispatch` and only
//! wakes when KWin has something to say. It reports through the shell bridge
//! (`wallpaperWindowsJson`), a JSON array of this pid's windows with their
//! absolute geometry, republished only when that array changes;
//! `shell/WallpaperField.qml` picks the one whose size is the
//! ApplicationWindow's and turns the global origin into a per-screen offset.
//! Nothing else in the app touches this connection.
//!
//! WHO GETS THE PROTOCOL. KWin blacklists `org_kde_plasma_window_management`
//! (`kwin/src/wayland_server.cpp`, `interfacesBlackList`) and hands it only to
//! a client whose executable — `/proc/<pid>/exe` — is named by a `.desktop`
//! file whose `Exec` (first word, canonical ABSOLUTE path) is that binary and
//! which lists the interface in `X-KDE-Wayland-Interfaces`
//! (`kwin/src/utils/serviceutils.h`). The packaged desktop entries carry both
//! (`packaging/linux/qbz.desktop`: `Exec=/usr/bin/qbz`); a development binary
//! run from the target directory gets the protocol only with a user-level
//! desktop file pointing at it. Installs whose running binary no desktop file
//! can name never see it: Flatpak and Snap (sandboxed), the AppImage (a
//! per-run mount point), Nix (the wrapper script is not the binary that
//! runs). When the global is absent the `bind`
//! fails, one line is logged, and the crop stays centred — the same picture as
//! before this module, never a broken one. Other Wayland compositors have no
//! equivalent protocol and take the same fallback.
//!
//! The tracker starts lazily (`start()`, first use of mode 3 on a Wayland
//! session, from QML which knows the QPA platform) and runs for the life of
//! the process: geometry events arrive only when a window moves or resizes,
//! so an idle tracker costs nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use cxx_qt_lib::QString;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_plasma::plasma_window_management::client::org_kde_plasma_window::{
    self, OrgKdePlasmaWindow,
};
use wayland_protocols_plasma::plasma_window_management::client::org_kde_plasma_window_management::{
    self, OrgKdePlasmaWindowManagement,
};

/// `window_with_uuid` arrived with v13; `client_geometry` (the surface
/// without decorations, which is what the ApplicationWindow measures) with
/// v18. Everything in between is tolerated: a v13–v17 compositor reports the
/// frame geometry instead, which is the same rectangle for QBZ's frameless
/// window.
const MIN_VERSION: u32 = 13;
const MAX_VERSION: u32 = 18;

static STARTED: AtomicBool = AtomicBool::new(false);

/// One window of this process, as the compositor reports it. Absolute
/// (global, logical) coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Everything the compositor has said about one mapped window, until its
/// `pid` proves it is not ours (then the proxy is destroyed and the entry
/// dropped).
#[derive(Debug, Default)]
struct Tracked {
    pid: Option<u32>,
    /// Frame geometry (`geometry`), the v13–v17 fallback.
    frame: Option<(i32, i32, u32, u32)>,
    /// Surface geometry (`client_geometry`, v18) — preferred.
    client: Option<(i32, i32, u32, u32)>,
}

struct State {
    me: u32,
    windows: HashMap<String, Tracked>,
    dirty: bool,
    /// What the bridge holds now: a move that ends where it started, or a
    /// geometry event that repeats the frame, republishes nothing.
    published: String,
}

/// Start the tracker once per process. Safe to call on every mode switch:
/// every call after the first returns immediately. Outside a Wayland session
/// there is nothing to track and nothing is started.
pub fn start() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("qbz-wl-windows".into())
        .spawn(run)
    {
        log::warn!("[qbz-qt] wallpaper: window tracker thread failed to start: {e}");
        STARTED.store(false, Ordering::SeqCst);
    }
}

fn run() {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[qbz-qt] wallpaper: no Wayland connection for the window tracker: {e}");
            return;
        }
    };
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[qbz-qt] wallpaper: Wayland registry failed: {e}");
            return;
        }
    };
    let qh = queue.handle();
    let _manager: OrgKdePlasmaWindowManagement =
        match globals.bind(&qh, MIN_VERSION..=MAX_VERSION, ()) {
            Ok(m) => m,
            Err(e) => {
                log::info!(
                    "[qbz-qt] wallpaper: org_kde_plasma_window_management is not offered to this \
                     binary ({e}); the crop stays centred (wallpaper_wayland_qt.rs)"
                );
                return;
            }
        };
    let mut state = State {
        me: std::process::id(),
        windows: HashMap::new(),
        dirty: false,
        published: "[]".to_string(),
    };
    loop {
        if let Err(e) = queue.blocking_dispatch(&mut state) {
            log::warn!("[qbz-qt] wallpaper: window tracker stopped: {e}");
            publish("[]".to_string());
            return;
        }
        if state.dirty {
            state.dirty = false;
            let json = to_json(&state.mine());
            if json != state.published {
                state.published = json.clone();
                publish(json);
            }
        }
    }
}

impl State {
    /// This process's windows, in a stable order (position, then size) so
    /// two publishes of the same layout serialise identically.
    fn mine(&self) -> Vec<WindowRect> {
        let mut out: Vec<WindowRect> = self
            .windows
            .values()
            .filter(|w| w.pid == Some(self.me))
            .filter_map(|w| {
                let (x, y, width, height) = w.client.or(w.frame)?;
                Some(WindowRect {
                    x,
                    y,
                    width,
                    height,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            (a.x, a.y, a.width, a.height).cmp(&(b.x, b.y, b.width, b.height))
        });
        out
    }
}

/// The JSON the shell bridge carries: `[{"x","y","w","h"}, …]`.
pub(crate) fn to_json(rects: &[WindowRect]) -> String {
    let items: Vec<serde_json::Value> = rects
        .iter()
        .map(|r| {
            serde_json::json!({
                "x": r.x,
                "y": r.y,
                "w": r.width,
                "h": r.height,
            })
        })
        .collect();
    serde_json::Value::Array(items).to_string()
}

fn publish(json: String) {
    crate::shell_bridge::ui(move |mut b| {
        b.as_mut()
            .set_wallpaper_windows_json(QString::from(json.as_str()));
    });
}

// The registry: `registry_queue_init` collected the globals; nothing to do
// per event.
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<OrgKdePlasmaWindowManagement, ()> for State {
    fn event(
        state: &mut Self,
        manager: &OrgKdePlasmaWindowManagement,
        event: org_kde_plasma_window_management::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // A mapped window: ask for its object, keyed by the uuid the
        // compositor gave it. Every window on the desktop announces itself
        // here; the `pid` event sorts ours from the rest.
        if let org_kde_plasma_window_management::Event::WindowWithUuid { uuid, .. } = event {
            manager.get_window_by_uuid(uuid.clone(), qh, uuid.clone());
            state.windows.insert(uuid, Tracked::default());
        }
    }
}

impl Dispatch<OrgKdePlasmaWindow, String> for State {
    fn event(
        state: &mut Self,
        window: &OrgKdePlasmaWindow,
        event: org_kde_plasma_window::Event,
        uuid: &String,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use org_kde_plasma_window::Event;
        let me = state.me;
        let Some(tracked) = state.windows.get_mut(uuid) else {
            return;
        };
        match event {
            Event::PidChanged { pid } => tracked.pid = Some(pid),
            Event::Geometry {
                x,
                y,
                width,
                height,
            } => {
                tracked.frame = Some((x, y, width, height));
                state.dirty |= tracked.pid == Some(me);
            }
            Event::ClientGeometry {
                x,
                y,
                width,
                height,
            } => {
                tracked.client = Some((x, y, width, height));
                state.dirty |= tracked.pid == Some(me);
            }
            // The compositor has said what it knows about a new window
            // (kwin sends `pid_changed` before this, `client_geometry`
            // after it). Someone else's: let go of it now, so its later
            // moves never reach this thread.
            Event::InitialState => {
                if tracked.pid != Some(me) {
                    state.windows.remove(uuid);
                    window.destroy();
                } else {
                    state.dirty = true;
                }
            }
            Event::Unmapped => {
                let mine = tracked.pid == Some(me);
                state.windows.remove(uuid);
                window.destroy();
                state.dirty |= mine;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_carries_each_window_with_short_keys() {
        let rects = vec![
            WindowRect {
                x: 640,
                y: 120,
                width: 1720,
                height: 980,
            },
            WindowRect {
                x: -10,
                y: 0,
                width: 320,
                height: 96,
            },
        ];
        let json = to_json(&rects);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["x"], 640);
        assert_eq!(arr[0]["y"], 120);
        assert_eq!(arr[0]["w"], 1720);
        assert_eq!(arr[0]["h"], 980);
        assert_eq!(arr[1]["x"], -10);
    }

    #[test]
    fn empty_tracker_publishes_an_empty_array() {
        assert_eq!(to_json(&[]), "[]");
    }

    #[test]
    fn only_this_process_windows_are_reported_and_client_geometry_wins() {
        let mut state = State {
            me: 4242,
            windows: HashMap::new(),
            dirty: false,
            published: String::new(),
        };
        state.windows.insert(
            "a".into(),
            Tracked {
                pid: Some(4242),
                frame: Some((100, 100, 800, 600)),
                client: Some((104, 130, 792, 566)),
            },
        );
        state.windows.insert(
            "b".into(),
            Tracked {
                pid: Some(7),
                frame: Some((0, 0, 500, 500)),
                client: None,
            },
        );
        state.windows.insert(
            "c".into(),
            Tracked {
                pid: Some(4242),
                frame: None,
                client: None,
            },
        );
        let mine = state.mine();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].x, 104);
        assert_eq!(mine[0].width, 792);
    }
}
