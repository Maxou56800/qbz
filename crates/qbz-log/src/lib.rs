//! `qbz-log` — frontend-agnostic logging core for the qbz desktop client.
//!
//! It owns a composite [`log::Log`] implementation ([`tee::TeeLogger`]) that wraps
//! `env_logger`'s built `Logger` and fans visible records out to three sinks:
//!   1. **stderr** (redacted text, same line format as the file sink),
//!   2. a bounded **in-memory ring** ([`ring`], cap [`ring::RING_CAP`]), and
//!   3. an **on-disk file** (`~/.local/share/qbz/logs/qbz.log`; `qbzd.log` for the daemon),
//!      prev-rotated at startup unless another live process is writing it.
//!
//! Secret **redaction** ([`redact`]) is applied once at the single write choke point,
//! so every downstream consumer (stderr, ring, file, clipboard, paste upload) gets clean text.
//! Consecutive identical redacted records retain their first line and periodic
//! count/duration summaries; any different visible record ends the run. All three
//! sinks receive the same serialized sequence. Call `log::logger().flush()` before
//! normal exit to emit a final pending summary and flush the file.
//!
//! This crate is network-free and UI-free: no `reqwest`, no `tokio`, no `slint`.

pub mod bundle;
pub mod fatal;
pub mod install;
pub mod line;
pub mod redact;
mod repeat;
pub mod ring;
pub mod tee;

pub use bundle::{format_diagnostics_bundle, DiagFields};
pub use fatal::install as install_fatal_signal_reporter;
pub use install::{install, install_named, install_without_file_sink, set_level};
pub use line::LogLine;
pub use redact::{redact, register_secret};
