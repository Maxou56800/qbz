//! The composite [`log::Log`] that fans every record to stderr, the ring, and the file.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Mutex;
use std::time::Instant;

use log::{Log, Metadata, Record};

use crate::install::FileSink;
use crate::line::LogLine;
use crate::repeat::ConsecutiveRecords;
use crate::{redact, ring};

/// Wraps `env_logger`'s built `Logger` and tees every record to the in-memory ring and
/// (optionally) the on-disk log file, with secret redaction applied once at this single
/// write choke point. **All** sinks (ring, file, and stderr) receive the redacted text.
/// Consecutive duplicate records are summarized with their original severity,
/// target, redacted message, repetition count and monotonic elapsed time.
pub struct TeeLogger {
    inner: env_logger::Logger,
    // One lock covers grouping AND all sink writes: concurrent threads cannot
    // interleave a summary and its next record or reorder the file vs. the ring.
    output: Mutex<Output>,
}

struct Output {
    file: Option<BufWriter<File>>,
    /// Held for the process lifetime: marks this process as a live writer of
    /// the log file (see `install::open_log_file_at`).
    _lock: Option<File>,
    consecutive: ConsecutiveRecords,
}

impl TeeLogger {
    pub(crate) fn new(inner: env_logger::Logger, sink: Option<FileSink>) -> Self {
        let (file, lock) = match sink {
            Some(sink) => (Some(sink.writer), sink.lock),
            None => (None, None),
        };
        Self {
            inner,
            output: Mutex::new(Output {
                file,
                _lock: lock,
                consecutive: ConsecutiveRecords::default(),
            }),
        }
    }
}

impl Output {
    fn write(&mut self, line: LogLine) {
        let formatted = format_line(&line);
        ring::push(line);
        if let Some(writer) = &mut self.file {
            let _ = writeln!(writer, "{formatted}");
        }
        // Never delegate to inner.log(record): it would bypass redaction.
        let _ = writeln!(std::io::stderr(), "{formatted}");
    }
}

fn now_epoch_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

/// Format a redacted log line the same way the file sink does (stable, greppable).
fn format_line(line: &LogLine) -> String {
    format!(
        "{} {:5} {} {}",
        line.format_ts(),
        line.level_str(),
        line.target,
        line.message
    )
}

impl Log for TeeLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        // Honor the inner logger's filter so the ring matches what stderr would show.
        if !self.inner.enabled(record.metadata()) {
            return;
        }

        // Redact ONCE; every downstream sink gets the cleaned text.
        let msg = redact::redact(&record.args().to_string());
        let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
        let line = LogLine {
            ts: now_epoch_ms(),
            level: record.level(),
            target: record.target().to_owned(),
            message: msg,
        };
        let now = Instant::now();
        let [summary, first] = output.consecutive.push(line, now);
        for line in [summary, first].into_iter().flatten() {
            output.write(line);
        }
        // Flush each emitted batch before returning, including the last INFO
        // before a hang. A time check on the next record cannot bound an idle
        // buffer. Duplicate compaction still avoids writes for repeated lines.
        if let Some(writer) = &mut output.file {
            let _ = writer.flush();
        }
    }

    fn flush(&self) {
        let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(summary) = output.consecutive.flush() {
            output.write(summary);
        }
        self.inner.flush();
        if let Some(writer) = &mut output.file {
            let _ = writer.flush();
        }
        let _ = std::io::stderr().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level;

    #[test]
    fn last_info_is_visible_without_another_record_or_explicit_flush() {
        // The real logger writes to the process-global ring. Isolate this
        // exercise from the ring unit test, which checks exact FIFO contents.
        const MARKER: &str = "QBZ_LOG_LAST_INFO_TEST_CHILD";
        if std::env::var_os(MARKER).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "tee::tests::last_info_is_visible_without_another_record_or_explicit_flush",
                    "--exact",
                ])
                .env(MARKER, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let path = std::env::temp_dir().join(format!("qbz-log-last-info-{}", std::process::id()));
        let file = File::create(&path).unwrap();
        let inner = env_logger::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .build();
        let logger = TeeLogger::new(
            inner,
            Some(FileSink {
                writer: BufWriter::new(file),
                lock: None,
            }),
        );
        logger.log(
            &Record::builder()
                .level(Level::Info)
                .target("test")
                .args(format_args!("last startup message"))
                .build(),
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("last startup message"));
        drop(logger);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn format_line_includes_redacted_message_not_raw() {
        let line = LogLine {
            ts: 0,
            level: Level::Info,
            target: "qbz".into(),
            message: "token=***REDACTED***".into(),
        };
        let s = format_line(&line);
        assert!(s.contains("***REDACTED***"));
        assert!(!s.contains("SEKRET"));
    }
}
