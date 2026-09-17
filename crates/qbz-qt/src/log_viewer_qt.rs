//! In-app log viewer — the read surface over `qbz_log::ring`.
//!
//! Port of `crates/qbz/src/log_viewer.rs`. The ring itself has been live in
//! this build since it landed (`main.rs` installs the tee at boot); what was
//! missing was any way to LOOK at it — Settings > "Share logs" called
//! `open::that(log_path)` and handed the user off to their file manager,
//! where the reference opens a filterable viewer with copy/bundle/upload.
//!
//! Nothing here holds the history: `refresh` snapshots the ring, filters by
//! level + search, keeps the last [`MAX_VIEW_ROWS`], and publishes THAT as a
//! JSON document. `total` is the ring size and `shown` is what survived the
//! filter, so the header can say "showing 200 of 4,000" the way the
//! reference's does.
//!
//! Filtering lives here rather than in QML for the same reason it lives in
//! Rust in the reference: the ring can hold thousands of lines and the view
//! shows at most 500, so filtering QML-side would ship the whole history
//! across the bridge on every keystroke.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use cxx_qt_lib::QString;
use serde::Serialize;

use crate::shell_bridge::ui;

/// The view cap. The reference uses the same number.
const MAX_VIEW_ROWS: usize = 500;
/// Lines carried in a shared/uploaded bundle.
const BUNDLE_LINES: usize = 200;

static OPEN: AtomicBool = AtomicBool::new(false);
static AUTO_TAIL: AtomicBool = AtomicBool::new(false);
static UPLOADING: AtomicBool = AtomicBool::new(false);
/// The copy button's transient confirmation. The reference flashes this on
/// the BUTTON (its label and glyph swap to "Copied" + a check) rather than
/// raising a toast — the modal is already the focus, so a toast would fire
/// behind it.
static COPIED: AtomicBool = AtomicBool::new(false);
static FILTER: LazyLock<Mutex<Filter>> = LazyLock::new(|| Mutex::new(Filter::default()));
static UPLOADED_URL: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

#[derive(Default, Clone)]
struct Filter {
    level: String,
    search: String,
}

#[derive(Serialize)]
struct Row {
    ts: String,
    level: String,
    target: String,
    message: String,
}

#[derive(Serialize)]
struct Doc {
    open: bool,
    rows: Vec<Row>,
    /// Ring size, before the filter.
    total: i32,
    /// After filter + cap.
    shown: i32,
    #[serde(rename = "filterLevel")]
    filter_level: String,
    search: String,
    #[serde(rename = "autoTail")]
    auto_tail: bool,
    uploading: bool,
    copied: bool,
    #[serde(rename = "uploadedUrl")]
    uploaded_url: String,
}

/// `all` matches everything; otherwise the line's level must match exactly.
/// The search is a case-insensitive substring over target + message, which is
/// what the reference matches on (`log_viewer.rs:169-185`).
fn line_matches(line: &qbz_log::LogLine, level: &str, search: &str) -> bool {
    if level != "all" && !line.level_str().eq_ignore_ascii_case(level) {
        return false;
    }
    if search.is_empty() {
        return true;
    }
    line.message.to_lowercase().contains(search) || line.target.to_lowercase().contains(search)
}

fn snapshot_doc() -> Doc {
    let f = FILTER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let level = if f.level.is_empty() {
        "all".to_string()
    } else {
        f.level.to_lowercase()
    };
    let search = f.search.to_lowercase();

    let snap = qbz_log::ring::snapshot();
    let total = snap.len();
    let filtered: Vec<&qbz_log::LogLine> = snap
        .iter()
        .filter(|l| line_matches(l, &level, &search))
        .collect();
    let start = filtered.len().saturating_sub(MAX_VIEW_ROWS);
    let rows: Vec<Row> = filtered[start..]
        .iter()
        .map(|l| Row {
            ts: l.format_ts(),
            level: l.level_str().to_string(),
            target: l.target.clone(),
            // Redacted defensively — the ring's write choke point already
            // redacts, but this text can leave the machine.
            message: qbz_log::redact(&l.message),
        })
        .collect();
    let shown = rows.len() as i32;

    Doc {
        open: OPEN.load(Ordering::SeqCst),
        rows,
        total: total as i32,
        shown,
        filter_level: level,
        search: f.search,
        auto_tail: AUTO_TAIL.load(Ordering::SeqCst),
        uploading: UPLOADING.load(Ordering::SeqCst),
        copied: COPIED.load(Ordering::SeqCst),
        uploaded_url: UPLOADED_URL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
    }
}

/// Flash the copy button's confirmation, then clear it.
fn flash_copied() {
    COPIED.store(true, Ordering::SeqCst);
    publish();
    crate::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        COPIED.store(false, Ordering::SeqCst);
        publish();
    });
}

pub fn publish() {
    let json = serde_json::to_string(&snapshot_doc()).unwrap_or_else(|_| "{}".into());
    ui(move |mut b| b.as_mut().set_log_viewer_json(QString::from(json.as_str())));
}

pub fn open() {
    OPEN.store(true, Ordering::SeqCst);
    publish();
}

pub fn close() {
    OPEN.store(false, Ordering::SeqCst);
    // Auto-tail must not survive the close, or a closed modal keeps waking
    // the UI thread twice a second for a document nobody reads.
    set_auto_tail(false);
    publish();
}

pub fn set_level(level: String) {
    FILTER.lock().unwrap_or_else(|e| e.into_inner()).level = level;
    publish();
}

pub fn set_search(search: String) {
    FILTER.lock().unwrap_or_else(|e| e.into_inner()).search = search;
    publish();
}

pub fn clear() {
    qbz_log::ring::clear();
    publish();
}

/// Repeating republish while the viewer is open and auto-tail is on.
///
/// One task per enable, guarded by the flag it watches — flipping auto-tail
/// off ends the loop on its next tick, and closing the modal flips it off.
pub fn set_auto_tail(on: bool) {
    let was = AUTO_TAIL.swap(on, Ordering::SeqCst);
    if on && !was {
        crate::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if !AUTO_TAIL.load(Ordering::SeqCst) || !OPEN.load(Ordering::SeqCst) {
                    return;
                }
                publish();
            }
        });
    }
    publish();
}

/// The filtered view as plain redacted lines — what "Copy" puts on the
/// clipboard.
fn filtered_text() -> String {
    // Export the partial repeat count without resetting every auto-tail refresh.
    log::logger().flush();
    snapshot_doc()
        .rows
        .iter()
        .map(|r| format!("{} {} {} {}", r.ts, r.level, r.target, r.message))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn copy_all() {
    crate::share_qt::copy_to_clipboard(filtered_text());
    flash_copied();
}

/// The GitHub-ready bundle: the shared formatter's `<details>` header, the FULL
/// diagnostics report, then the last [`BUNDLE_LINES`] lines.
///
/// This used to carry the eight header fields alone. The reference's bundle
/// (`crates/qbz/src/log_viewer.rs:260-277`) prepends the diagnostics report as
/// well — roughly seventy fields — so until the panel landed every issue filed
/// from a Qt build arrived with no kernel, no distro, no install method, no
/// GPU, no exclusive-mode / DAC / sample-rate state and no playback context.
/// [`crate::diagnostics_qt::report_markdown`] is that body, and this is the day
/// the narrowing ends.
///
/// It is injected INTO `format_diagnostics_bundle` rather than replacing it:
/// the reference's `build_share_text` emits no `<details><summary>` wrapper, and
/// dropping it would make a pasted bundle expand the whole log inline in the
/// GitHub issue.
///
/// ASYNC, and that is load-bearing: the report re-runs the settings reads, the
/// `pactl` shell-outs and the CPAL enumeration, so every caller has to be on
/// the tokio runtime — which is why `copy_bundle` is async and its invokable
/// takes the `log_upload` shape. The clipboard therefore lands one tick after
/// the click.
async fn bundle_text() -> String {
    log::logger().flush();
    let lines = qbz_log::ring::snapshot();
    let report = crate::diagnostics_qt::report_markdown().await;
    let fields = qbz_log::bundle::DiagFields {
        app_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        desktop: &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        session: &std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        audio_backend: &crate::settings_qt::current_backend_label(),
        locale: &qbz_i18n::current_language(),
        log_level: &std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    };
    qbz_log::bundle::format_diagnostics_bundle_with_report(&fields, &report, &lines, BUNDLE_LINES)
}

pub async fn copy_bundle() {
    crate::share_qt::copy_to_clipboard(bundle_text().await);
    flash_copied();
}

/// Upload the bundle to a public paste and publish the URL.
///
/// Same endpoint as the reference (paste.rs). The text is already redacted
/// twice over — at the ring's write choke point and again in `snapshot_doc` /
/// the bundle formatter — because this is the one path that leaves the
/// machine.
pub async fn upload() {
    if UPLOADING.swap(true, Ordering::SeqCst) {
        return; // already in flight
    }
    UPLOADED_URL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    publish();

    let body = bundle_text().await;
    let client = qbz_net_proxy::apply_current(reqwest::Client::builder())
        .and_then(|builder| builder.build().map_err(Into::into))
        .unwrap_or_default();
    let url = match client.post("https://paste.rs/").body(body).send().await {
        Ok(resp) => resp.text().await.unwrap_or_default().trim().to_string(),
        Err(e) => {
            log::warn!("[qbz-qt] log upload failed: {e}");
            String::new()
        }
    };
    *UPLOADED_URL.lock().unwrap_or_else(|e| e.into_inner()) = url.clone();
    UPLOADING.store(false, Ordering::SeqCst);
    if url.is_empty() {
        crate::toast_qt::error(qbz_i18n::t("Upload failed."));
    }
    publish();
}

pub fn copy_url() {
    let url = UPLOADED_URL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if url.is_empty() {
        return;
    }
    crate::share_qt::copy_to_clipboard(url);
    crate::toast_qt::success(qbz_i18n::t("Copied"));
}

pub fn open_log_file() {
    match qbz_log::install::log_file_path() {
        Some(path) => {
            if let Err(e) = open::that(&path) {
                log::warn!("[qbz-qt] open log file failed: {e}");
                crate::toast_qt::error(qbz_i18n::t("Could not open the log file."));
            }
        }
        None => crate::toast_qt::error(qbz_i18n::t("Could not open the log file.")),
    }
}
