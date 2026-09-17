//! One-shot logger installation + the on-disk file sink (open / rotate).

use std::fs::{File, OpenOptions, TryLockError};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::tee::TeeLogger;

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The desktop app's log file name.
pub const DEFAULT_LOG_FILE: &str = "qbz.log";

/// The on-disk sink plus the advisory lock that marks this process as a live
/// writer of that file (see [`open_log_file_at`]). Dropping it releases both.
pub(crate) struct FileSink {
    pub(crate) writer: BufWriter<File>,
    pub(crate) lock: Option<File>,
}

/// Install the [`TeeLogger`] as the global `log` logger, writing to
/// [`DEFAULT_LOG_FILE`].
///
/// Builds the inner `env_logger` logger from `RUST_LOG` (falling back to `default_level`),
/// opens/rotates the on-disk file, then sets the boxed logger + max level. Idempotent:
/// a second call is a guarded no-op (it neither rotates the file again nor panics).
pub fn install(default_level: &str) {
    install_with_file_sink(default_level, Some(DEFAULT_LOG_FILE));
}

/// Same as [`install`], but to `file_name` inside the same logs directory.
/// `qbzd` uses `qbzd.log` so a daemon never shares (or rotates) the desktop
/// app's file.
pub fn install_named(default_level: &str, file_name: &str) {
    install_with_file_sink(default_level, Some(file_name));
}

/// Same as [`install`], but with the on-disk file sink DISABLED (stderr + ring only).
///
/// For an internal, disposable child process that re-enters the same `main`
/// (presentation / GPU preflight, #749). A child's output is already captured
/// by the parent, so it loses nothing by staying off the file — and it never
/// has to touch the parent's file at all.
pub fn install_without_file_sink(default_level: &str) {
    install_with_file_sink(default_level, None);
}

fn install_with_file_sink(default_level: &str, file_name: Option<&str>) {
    // True one-shot guard: avoid re-rotating the log file or fighting an already-set logger.
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    // Demote chatty foreign crates in the DEFAULT filter: zbus 5 logs every
    // D-Bus message it dispatches/reads as multi-KB Debug dumps at INFO (via
    // the tracing-log bridge, which also emits `tracing::span` events). On a
    // desktop with an MPRIS applet polling GetAll this flooded the file sink
    // within ~1s of startup and drowned real entries (field-confirmed twice
    // in #555 logs) — and each suppressed record now costs nothing, since
    // `log!` checks the filter before formatting. An explicit RUST_LOG still
    // replaces the whole default, so full zbus tracing stays one env var away.
    // discord_rich_presence warns on every failed IPC connect ("find_pipe:
    // could not find pipe"); qbz-integrations logs its own single line and
    // backs off, so the crate's copy only needs to surface real errors.
    let inner = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(format!(
            "{default_level},zbus=warn,tracing=warn,discord_rich_presence=error"
        )),
    )
    .build();
    let level = inner.filter();
    let sink = file_name.and_then(open_log_file);

    // Ignore the Err if a logger was somehow already set elsewhere.
    if log::set_boxed_logger(Box::new(TeeLogger::new(inner, sink))).is_ok() {
        log::set_max_level(level);
    }
}

/// Runtime log-level toggle (e.g. info <-> debug) with no restart.
pub fn set_level(level: log::LevelFilter) {
    log::set_max_level(level);
}

/// `~/.local/share/qbz/logs`, if a data dir exists.
fn logs_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("qbz").join("logs"))
}

/// Path to the desktop app's current-run log file (`~/.local/share/qbz/logs/qbz.log`).
pub fn log_file_path() -> Option<PathBuf> {
    log_file_path_named(DEFAULT_LOG_FILE)
}

/// Path to `file_name` inside the logs directory (`qbzd.log` for the daemon).
pub fn log_file_path_named(file_name: &str) -> Option<PathBuf> {
    Some(logs_dir()?.join(file_name))
}

fn open_log_file(file_name: &str) -> Option<FileSink> {
    open_log_file_at(&log_file_path_named(file_name)?)
}

/// Open the run's log file at `path`.
///
/// Rotation (`<name>` → `<name>.prev`, then a fresh file) happens ONLY when no
/// live process is writing the file. Liveness is an advisory lock on the
/// sidecar `<name>.lock`: every writer holds it shared for its lifetime; an
/// opener probes it with a non-blocking exclusive lock and, when that is
/// refused, APPENDS to the live file instead of renaming it away. That is
/// what a second `qbz` launch, a `qbzd` sharing the directory, or any future
/// caller of [`install`] used to do to a running process (#749, and the
/// 2026-09 static review). The lock lives on the sidecar, never on the log
/// itself, so it can never interfere with writes (a shared `LockFileEx`
/// would, on Windows). Returns `None` (file sink disabled, gracefully) on any
/// filesystem error.
fn open_log_file_at(path: &Path) -> Option<FileSink> {
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();

    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(dir.join(format!("{name}.lock")))
        .ok();
    let live_writer = match lock.as_ref().map(File::try_lock) {
        Some(Err(TryLockError::WouldBlock)) => true,
        // Granted, unsupported (exotic filesystem) or no sidecar: nobody we
        // can see is live — keep the historical rotate behaviour.
        Some(Ok(())) | Some(Err(TryLockError::Error(_))) | None => false,
    };
    if let Some(lock) = lock.as_ref() {
        // Join the live writers (downgrade the probe if we held it).
        let _ = lock.unlock();
        let _ = lock.lock_shared();
    }

    let file = if live_writer && path.exists() {
        OpenOptions::new().append(true).open(path).ok()?
    } else {
        if path.exists() {
            // Best-effort rotation; a failure here must not disable logging.
            let _ = std::fs::rename(path, dir.join(format!("{name}.prev")));
        }
        File::create(path).ok()?
    };
    Some(FileSink {
        writer: BufWriter::new(file),
        lock,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qbz-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// #749 was a child process renaming the live log away; a second `qbz`
    /// launch or a `qbzd` in the same directory did the same. A live writer
    /// holds the sidecar lock, so a second opener APPENDS.
    #[test]
    fn a_second_open_while_the_first_writer_is_live_appends() {
        let dir = fresh_dir("live");
        let path = dir.join("qbz.log");

        let mut first = open_log_file_at(&path).expect("first open");
        writeln!(first.writer, "parent line").unwrap();
        first.writer.flush().unwrap();

        let mut second = open_log_file_at(&path).expect("second open");
        writeln!(second.writer, "second-instance line").unwrap();
        second.writer.flush().unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "parent line\nsecond-instance line\n"
        );
        assert!(!dir.join("qbz.log.prev").exists(), "nothing was rotated");
        drop(second);
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no live writer the historical contract holds: rotate to
    /// `<name>.prev` and start fresh. The prev name follows the file name.
    #[test]
    fn open_after_the_previous_writer_closed_rotates_to_prev() {
        let dir = fresh_dir("rotate");
        let path = dir.join("qbzd.log");
        {
            let mut first = open_log_file_at(&path).expect("first open");
            writeln!(first.writer, "run one").unwrap();
            first.writer.flush().unwrap();
        }
        let mut second = open_log_file_at(&path).expect("second open");
        writeln!(second.writer, "run two").unwrap();
        second.writer.flush().unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "run two\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("qbzd.log.prev")).unwrap(),
            "run one\n"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_log_paths_share_the_logs_directory() {
        let Some(default) = log_file_path() else { return };
        let daemon = log_file_path_named("qbzd.log").unwrap();
        assert_eq!(default.parent(), daemon.parent());
        assert_eq!(default.file_name().unwrap(), DEFAULT_LOG_FILE);
        assert_eq!(daemon.file_name().unwrap(), "qbzd.log");
    }
}
