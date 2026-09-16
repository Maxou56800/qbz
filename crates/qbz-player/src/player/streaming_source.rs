//! Buffered media source for streaming playback.
//!
//! Provides two main components:
//! 1. `BufferedMediaSource` - Wraps an async HTTP response to provide a synchronous
//!    `Read + Seek` interface required by symphonia decoders.
//! 2. `IncrementalStreamingSource` - A rodio Source that decodes audio packets
//!    incrementally as they become available, allowing playback to start before
//!    the entire file is downloaded.
//!
//! # Design
//!
//! The source uses a growing buffer that accumulates data from the HTTP response.
//! - Reads block if requesting data not yet buffered
//! - Seek forward blocks until data is available
//! - Seek backward works within buffered data
//! - Seek beyond current buffer position blocks until data arrives
//!
//! # Thread Safety
//!
//! The buffer state is shared between:
//! - The reader (audio thread, synchronous)
//! - The writer (download task, async)
//!
//! Communication uses `Mutex` + `Condvar` for blocking synchronization.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Cursor, Error as IoError, ErrorKind, Read, Result as IoResult, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use rodio::Source;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

use super::{PlaybackBufferReporter, PlaybackBufferState};

/// Configuration for the streaming buffer
#[derive(Debug, Clone)]
pub struct StreamingConfig {
    /// Minimum bytes to buffer before allowing reads (for format detection)
    pub initial_buffer_bytes: usize,
    /// Maximum buffer size before backpressure (not enforced, just for info)
    pub max_buffer_bytes: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            // 512KB default - enough for format headers and ~2-5 seconds of audio
            // This allows playback to start quickly while still having enough
            // buffer to handle network jitter
            initial_buffer_bytes: 512 * 1024,
            // 100MB max buffer
            max_buffer_bytes: 100 * 1024 * 1024,
        }
    }
}

impl StreamingConfig {
    /// Create config from buffer seconds and approximate bitrate
    ///
    /// For Hi-Res FLAC at 192kHz/24bit stereo, bitrate is roughly 9.2 Mbps
    /// We estimate ~1MB per second as a conservative approximation
    pub fn from_seconds(seconds: u8) -> Self {
        // Minimum 256KB to ensure format detection works
        let bytes = ((seconds as usize) * 1024 * 1024).max(256 * 1024);
        Self {
            initial_buffer_bytes: bytes,
            max_buffer_bytes: 100 * 1024 * 1024,
        }
    }

    /// Create a minimal config for fastest startup
    /// Uses smallest buffer that still allows format detection (~256KB)
    pub fn fast_start() -> Self {
        Self {
            initial_buffer_bytes: 256 * 1024,
            max_buffer_bytes: 100 * 1024 * 1024,
        }
    }

    /// Create config dynamically based on measured download speed
    ///
    /// - Very fast (>10 MB/s): 256KB (instant start)
    /// - Fast (5-10 MB/s): 384KB
    /// - Normal (2-5 MB/s): 512KB
    /// - Slow (1-2 MB/s): 1MB (more buffer to prevent stutter)
    /// - Very slow (<1 MB/s): 2MB
    ///
    /// Result is clamped to the process-wide cap configured via
    /// [`set_max_initial_buffer_bytes`] (typically derived from the host's
    /// memory profile — see qbz-core's system_capabilities). On
    /// memory-constrained hosts the slow-connection branches would
    /// otherwise inflate to 2 MB, which is exactly the wrong direction
    /// when "slow connection" is itself a symptom of swap thrash
    /// (issue #331, Pi 3B).
    pub fn from_speed_mbps(speed_mbps: f64) -> Self {
        let cap = MAX_INITIAL_BUFFER_BYTES.load(std::sync::atomic::Ordering::Relaxed);
        let cfg = Self::from_speed_mbps_with_cap(speed_mbps, cap);

        if cfg.initial_buffer_bytes < raw_initial_buffer_for_speed(speed_mbps) {
            log::info!(
                "Dynamic buffer: {:.1} MB/s detected → {}KB (capped from {}KB by host memory profile)",
                speed_mbps,
                cfg.initial_buffer_bytes / 1024,
                raw_initial_buffer_for_speed(speed_mbps) / 1024
            );
        } else {
            log::info!(
                "Dynamic buffer: {:.1} MB/s detected → {}KB initial buffer",
                speed_mbps,
                cfg.initial_buffer_bytes / 1024
            );
        }

        cfg
    }

    /// Pure variant of [`from_speed_mbps`] — derives the speed-based
    /// initial buffer and clamps to `cap` without touching global state
    /// or logging. Exposed for unit tests; production callers should use
    /// `from_speed_mbps`, which reads the process-wide cap.
    pub fn from_speed_mbps_with_cap(speed_mbps: f64, cap: usize) -> Self {
        let raw_initial_buffer = raw_initial_buffer_for_speed(speed_mbps);
        Self {
            initial_buffer_bytes: raw_initial_buffer.min(cap),
            max_buffer_bytes: 100 * 1024 * 1024,
        }
    }
}

/// Speed-driven initial buffer size, before any cap is applied.
/// Pure function — used by both `from_speed_mbps` and
/// `from_speed_mbps_with_cap` so they share the same ladder.
fn raw_initial_buffer_for_speed(speed_mbps: f64) -> usize {
    if speed_mbps >= 10.0 {
        256 * 1024 // 256KB - instant start for very fast connections
    } else if speed_mbps >= 5.0 {
        384 * 1024 // 384KB
    } else if speed_mbps >= 2.0 {
        512 * 1024 // 512KB - default
    } else if speed_mbps >= 1.0 {
        1024 * 1024 // 1MB - more buffer for slower connections
    } else {
        2 * 1024 * 1024 // 2MB - maximum buffer for very slow connections
    }
}

/// Process-wide cap for dynamically-derived initial buffer sizes.
/// Defaults to `usize::MAX` (no cap) so behavior is unchanged unless the
/// host explicitly configures it via [`set_max_initial_buffer_bytes`] —
/// typically once at process start, derived from the detected memory
/// profile.
static MAX_INITIAL_BUFFER_BYTES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

/// Set the process-wide cap for `StreamingConfig::from_speed_mbps`.
/// Subsequent calls to that constructor clamp their result to this cap.
pub fn set_max_initial_buffer_bytes(bytes: usize) {
    MAX_INITIAL_BUFFER_BYTES.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// Read the current cap. Mainly useful for tests.
pub fn max_initial_buffer_bytes() -> usize {
    MAX_INITIAL_BUFFER_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Absolute ceiling for the up-front buffer reservation: 1 GiB. Guards
/// against absurd Content-Length values reserving more RAM than any real
/// track needs.
const MAX_PREALLOC_BYTES: u64 = 1024 * 1024 * 1024;

/// Pure decision for the up-front reservation in [`BufferedMediaSource::new`]:
/// `Some(capacity)` when `total_size` is known and non-zero (capped at
/// [`MAX_PREALLOC_BYTES`]), `None` when the caller should fall back to the
/// config's initial-buffer capacity.
fn prealloc_capacity(total_size: Option<u64>) -> Option<usize> {
    let total = total_size?;
    if total == 0 {
        return None;
    }
    Some(total.min(MAX_PREALLOC_BYTES) as usize)
}

/// Internal state shared between reader and writer
struct BufferState {
    /// Accumulated data from HTTP response
    data: Arc<Vec<u8>>,
    file: Option<Arc<File>>,
    written: usize,
    /// True when HTTP download is complete
    download_complete: bool,
    /// Error from download, if any
    download_error: Option<String>,
    /// Total expected size (from Content-Length), if known
    total_size: Option<u64>,
}

impl BufferState {
    fn len(&self) -> usize {
        if self.file.is_some() {
            self.written
        } else {
            self.data.len()
        }
    }
}

// Positional I/O keeps cloned decoder readers independent and never holds the
// buffer-state mutex across disk operations. Writers publish only committed bytes.
fn file_read_at(file: &File, out: &mut [u8], offset: u64) -> IoResult<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_at(out, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        file.seek_read(out, offset)
    }
}

fn file_write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> IoResult<()> {
    while !bytes.is_empty() {
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::FileExt;
            file.write_at(bytes, offset)
        };
        #[cfg(windows)]
        let result = {
            use std::os::windows::fs::FileExt;
            file.seek_write(bytes, offset)
        };
        match result {
            Ok(0) => {
                return Err(IoError::new(
                    ErrorKind::WriteZero,
                    "stream spool write returned zero",
                ))
            }
            Ok(n) => {
                bytes = &bytes[n..];
                offset += n as u64;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// A media source that buffers from an async HTTP stream.
///
/// Provides `Read + Seek` interface for decoders while data is still downloading.
/// The source is created with a `BufferWriter` that receives chunks from the
/// download task.
pub struct BufferedMediaSource {
    state: Arc<(Mutex<BufferState>, Condvar)>,
    config: StreamingConfig,
    /// Each reader has its own read position
    read_pos: std::sync::atomic::AtomicU64,
    reader_generation: Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
}

impl BufferedMediaSource {
    /// Create a new buffered source.
    ///
    /// Returns the source and a writer for pushing downloaded chunks.
    /// The writer should be used from the async download task.
    pub fn new(config: StreamingConfig, total_size: Option<u64>) -> (Self, BufferWriter) {
        Self::new_storage(config, total_size, None)
    }

    /// Use a temporary disk file as the growing stream, retaining only bounded
    /// decoder buffers in RAM. The caller owns the choice of disk and policy.
    pub fn new_on_disk(
        config: StreamingConfig,
        total_size: Option<u64>,
        file: File,
    ) -> (Self, BufferWriter) {
        Self::new_storage(config, total_size, Some(file))
    }

    /// A completed L2 cache file uses the same decoder, seeking and resume path
    /// as an in-flight stream. The open handle survives cache eviction.
    pub fn from_file(file: File) -> IoResult<Self> {
        let size = usize::try_from(file.metadata()?.len()).map_err(|_| {
            IoError::new(
                ErrorKind::InvalidData,
                "audio file exceeds addressable stream size",
            )
        })?;
        let (source, _) = Self::new_on_disk(StreamingConfig::fast_start(), Some(size as u64), file);
        {
            let mut state = source.state.0.lock().unwrap();
            state.written = size;
            state.download_complete = true;
        }
        Ok(source)
    }

    pub fn is_file_backed(&self) -> bool {
        self.state
            .0
            .lock()
            .map(|state| state.file.is_some())
            .unwrap_or(false)
    }

    fn new_storage(
        config: StreamingConfig,
        total_size: Option<u64>,
        file: Option<File>,
    ) -> (Self, BufferWriter) {
        // When the total track size is known, reserve it up front: the
        // buffer ends up holding the whole track either way, so this skips
        // the ~8 doubling reallocs (and their memcpy) a 60-400 MB download
        // would otherwise pay. Falls back to the initial-buffer capacity
        // when the reservation fails (e.g. a 424 MB track on 32-bit, where
        // the contiguous range may not exist) or the size is unknown.
        let data = if file.is_some() {
            Vec::new()
        } else {
            match prealloc_capacity(total_size) {
                Some(cap) => {
                    let mut v = Vec::new();
                    if v.try_reserve_exact(cap).is_err() {
                        log::warn!(
                        "Stream buffer: could not reserve {} bytes up front; growing dynamically",
                        cap
                    );
                        v = Vec::with_capacity(config.initial_buffer_bytes);
                    }
                    v
                }
                None => Vec::with_capacity(config.initial_buffer_bytes),
            }
        };

        let state = Arc::new((
            Mutex::new(BufferState {
                data: Arc::new(data),
                file: file.map(Arc::new),
                written: 0,
                download_complete: false,
                download_error: None,
                total_size,
            }),
            Condvar::new(),
        ));

        let source = Self {
            state: Arc::clone(&state),
            config: config.clone(),
            read_pos: std::sync::atomic::AtomicU64::new(0),
            reader_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            generation: 0,
        };

        let writer = BufferWriter {
            state,
            append: Arc::new(Mutex::new(())),
        };

        (source, writer)
    }

    /// Create a new reader that shares the same buffer but has its own read position.
    /// This is used to pass to symphonia which needs ownership of the reader.
    pub fn create_reader(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            config: self.config.clone(),
            read_pos: std::sync::atomic::AtomicU64::new(0),
            reader_generation: self.reader_generation.clone(),
            generation: self.reader_generation.load(std::sync::atomic::Ordering::SeqCst),
        }
    }

    /// Wake decoders being dropped without truncating/cancelling the download.
    /// A fresh reader used by Resume belongs to the next generation.
    pub(super) fn interrupt_readers(&self) {
        let (lock, cvar) = &*self.state;
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        self.reader_generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        cvar.notify_all();
    }

    /// Wait until initial buffer is filled or download completes.
    ///
    /// This should be called before passing the source to the decoder,
    /// to ensure enough data is available for format detection.
    ///
    /// Returns error if download fails before initial buffer is filled.
    pub fn wait_for_initial_buffer(&self) -> IoResult<()> {
        let (lock, cvar) = &*self.state;
        let mut state = lock
            .lock()
            .map_err(|_| IoError::new(ErrorKind::Other, "Failed to acquire buffer lock"))?;

        while state.len() < self.config.initial_buffer_bytes
            && !state.download_complete
            && state.download_error.is_none()
        {
            state = cvar
                .wait(state)
                .map_err(|_| IoError::new(ErrorKind::Other, "Condition variable wait failed"))?;
        }

        if let Some(ref err) = state.download_error {
            return Err(IoError::new(ErrorKind::Other, err.clone()));
        }

        Ok(())
    }

    /// Check if download is complete (full file in buffer)
    pub fn is_complete(&self) -> bool {
        let (lock, _) = &*self.state;
        if let Ok(state) = lock.lock() {
            state.download_complete && state.download_error.is_none()
        } else {
            false
        }
    }

    /// Get current buffer size in bytes
    pub fn buffer_size(&self) -> usize {
        let (lock, _) = &*self.state;
        if let Ok(state) = lock.lock() {
            state.len()
        } else {
            0
        }
    }

    /// Get the complete data if download finished successfully.
    ///
    /// Used to store in cache after streaming playback completes.
    /// Returns None if download is not complete or failed.
    ///
    /// Legacy copying interface. The completed allocation is shared under
    /// the mutex; the copy happens after releasing it. Promotion uses
    /// `complete_data_shared` directly. Never take the active reader's bytes.
    pub fn take_complete_data(&self) -> Option<Vec<u8>> {
        self.complete_data_shared().map(|data| (*data).clone())
    }

    /// Completed buffers are immutable and can be shared without copying or
    /// taking bytes away from the active decoder. The reader lock is held only
    /// long enough to clone an Arc, regardless of track size.
    pub fn complete_data_shared(&self) -> Option<Arc<Vec<u8>>> {
        let (lock, _) = &*self.state;
        if let Ok(state) = lock.lock() {
            if state.download_complete && state.download_error.is_none() && state.file.is_none() {
                Some(Arc::clone(&state.data))
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Get a copy of the currently buffered data (for metadata extraction).
    ///
    /// Returns whatever data has been downloaded so far, even if incomplete.
    /// Useful for extracting file-level metadata (e.g., ReplayGain tags)
    /// which are typically in the first few KB of the file.
    pub fn get_buffered_data(&self) -> Option<Vec<u8>> {
        // File-level tags live in the prefix, not in hundreds of MiB of frames.
        const METADATA_LIMIT: usize = 1024 * 1024;
        let state = self.state.0.lock().ok()?;
        let count = state.len().min(METADATA_LIMIT);
        if count == 0 {
            return None;
        }
        if let Some(file) = state.file.clone() {
            drop(state);
            let mut prefix = vec![0; count];
            let mut offset = 0;
            while offset < count {
                let n = file_read_at(&file, &mut prefix[offset..], offset as u64).ok()?;
                if n == 0 {
                    return None;
                }
                offset += n;
            }
            Some(prefix)
        } else {
            Some(state.data[..count].to_vec())
        }
    }

    /// Get download progress as a fraction (0.0 to 1.0)
    ///
    /// Returns None if total size is unknown
    pub fn progress(&self) -> Option<f32> {
        let (lock, _) = &*self.state;
        if let Ok(state) = lock.lock() {
            state.total_size.map(|total| {
                if total == 0 {
                    1.0
                } else {
                    state.len() as f32 / total as f32
                }
            })
        } else {
            None
        }
    }

    /// Check if minimum buffer for playback is available
    ///
    /// Returns true when initial_buffer_bytes have been buffered
    /// or the download is complete.
    pub fn has_min_buffer(&self) -> bool {
        let (lock, _) = &*self.state;
        if let Ok(state) = lock.lock() {
            state.len() >= self.config.initial_buffer_bytes || state.download_complete
        } else {
            false
        }
    }

    /// Error reported by the feeder, if any. Lets waiters (the initial
    /// buffer fill loop) bail out immediately instead of sitting through
    /// the full buffer timeout when the feeder has already died.
    pub fn download_error(&self) -> Option<String> {
        let (lock, _) = &*self.state;
        lock.lock().ok().and_then(|s| s.download_error.clone())
    }
}

impl Read for BufferedMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        use std::sync::atomic::Ordering;

        let (lock, cvar) = &*self.state;
        let mut state = lock
            .lock()
            .map_err(|_| IoError::new(ErrorKind::Other, "Failed to acquire buffer lock"))?;

        let read_pos = self.read_pos.load(Ordering::SeqCst) as usize;

        // Wait for data if we're ahead of buffer
        while read_pos >= state.len() && !state.download_complete && state.download_error.is_none()
            && self.reader_generation.load(Ordering::SeqCst) == self.generation
        {
            state = cvar
                .wait(state)
                .map_err(|_| IoError::new(ErrorKind::Other, "Condition variable wait failed"))?;
        }

        if self.reader_generation.load(Ordering::SeqCst) != self.generation {
            return Ok(0);
        }
        // Check for errors
        if let Some(ref err) = state.download_error {
            return Err(IoError::new(ErrorKind::Other, err.clone()));
        }

        // EOF if at end and download complete
        if read_pos >= state.len() && state.download_complete {
            return Ok(0);
        }

        // Read available data
        let available = state.len() - read_pos;
        let to_read = buf.len().min(available);
        let to_read = if let Some(file) = state.file.clone() {
            drop(state);
            file_read_at(&file, &mut buf[..to_read], read_pos as u64)?
        } else {
            buf[..to_read].copy_from_slice(&state.data[read_pos..read_pos + to_read]);
            to_read
        };
        self.read_pos
            .store((read_pos + to_read) as u64, Ordering::SeqCst);

        Ok(to_read)
    }
}

impl Seek for BufferedMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> IoResult<u64> {
        use std::sync::atomic::Ordering;

        let (lock, cvar) = &*self.state;
        let mut state = lock
            .lock()
            .map_err(|_| IoError::new(ErrorKind::Other, "Failed to acquire buffer lock"))?;

        let current_pos = self.read_pos.load(Ordering::SeqCst) as i64;

        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => current_pos + offset,
            SeekFrom::End(offset) => {
                // For End seeks, we need to know total size or have complete download
                if let Some(total) = state.total_size {
                    total as i64 + offset
                } else if state.download_complete {
                    state.len() as i64 + offset
                } else {
                    // Can't seek from end without knowing size
                    return Err(IoError::new(
                        ErrorKind::Unsupported,
                        "Cannot seek from end while streaming without known size",
                    ));
                }
            }
        };

        if new_pos < 0 {
            return Err(IoError::new(
                ErrorKind::InvalidInput,
                "Seek position before start of stream",
            ));
        }

        let new_pos_usize = new_pos as usize;

        // If seeking forward beyond buffer, wait for data
        while new_pos_usize > state.len()
            && !state.download_complete
            && state.download_error.is_none()
        {
            state = cvar
                .wait(state)
                .map_err(|_| IoError::new(ErrorKind::Other, "Condition variable wait failed"))?;
        }

        if let Some(ref err) = state.download_error {
            return Err(IoError::new(ErrorKind::Other, err.clone()));
        }

        // After download complete, check bounds
        if state.download_complete && new_pos_usize > state.len() {
            return Err(IoError::new(
                ErrorKind::InvalidInput,
                "Seek position beyond end of stream",
            ));
        }

        self.read_pos.store(new_pos as u64, Ordering::SeqCst);
        Ok(new_pos as u64)
    }
}

// Required for symphonia MediaSource trait
impl MediaSource for BufferedMediaSource {
    fn is_seekable(&self) -> bool {
        // We support seeking within buffered data
        true
    }

    fn byte_len(&self) -> Option<u64> {
        let (lock, _) = &*self.state;
        if let Ok(state) = lock.lock() {
            state.total_size
        } else {
            None
        }
    }
}

/// Writer half for pushing downloaded chunks from the async download task.
///
/// This is the sender side that receives data from the HTTP response
/// and makes it available to the `BufferedMediaSource` reader.
#[derive(Clone)]
pub struct BufferWriter {
    state: Arc<(Mutex<BufferState>, Condvar)>,
    append: Arc<Mutex<()>>,
}

impl BufferWriter {
    pub fn complete_data_shared(&self) -> Option<Arc<Vec<u8>>> {
        let state = self.state.0.lock().ok()?;
        (state.download_complete && state.download_error.is_none() && state.file.is_none())
            .then(|| Arc::clone(&state.data))
    }

    /// Push a chunk of downloaded data
    ///
    /// This wakes up any readers waiting for data.
    pub fn push_chunk(&self, chunk: &[u8]) -> Result<(), String> {
        let _append = self
            .append
            .lock()
            .map_err(|_| "Failed to acquire stream append lock")?;
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().map_err(|_| "Failed to acquire buffer lock")?;
        if state.download_complete || state.download_error.is_some() {
            return Err("Cannot append to a completed or failed stream".into());
        }
        if let Some(file) = state.file.clone() {
            let offset = state.written;
            drop(state);
            if let Err(error) = file_write_all_at(&file, chunk, offset as u64) {
                let message = format!("stream spool write failed: {error}");
                let _ = self.error(message.clone());
                return Err(message);
            }
            state = lock.lock().map_err(|_| "Failed to acquire buffer lock")?;
            if state.download_complete || state.download_error.is_some() {
                return Err("Stream was sealed during disk append".into());
            }
            state.written = offset
                .checked_add(chunk.len())
                .ok_or("Stream size overflow")?;
        } else {
            Arc::get_mut(&mut state.data)
                .ok_or("Streaming buffer shared before completion")?
                .extend_from_slice(chunk);
        }
        cvar.notify_all();
        Ok(())
    }

    /// Mark download as complete
    ///
    /// After this is called, readers will receive EOF after reading all buffered data.
    pub fn complete(&self) -> Result<(), String> {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().map_err(|_| "Failed to acquire buffer lock")?;

        state.download_complete = true;
        cvar.notify_all();

        Ok(())
    }

    /// Mark download as failed
    ///
    /// After this is called, readers will receive the error on next read.
    /// The first recorded error wins: it is the root cause, and the feeder
    /// fail-guards fire a generic "aborted" error on drop after a specific
    /// failure has already been recorded, which must not overwrite it.
    pub fn error(&self, err: String) -> Result<(), String> {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().map_err(|_| "Failed to acquire buffer lock")?;

        if state.download_error.is_none() {
            state.download_error = Some(err);
        }
        cvar.notify_all();

        Ok(())
    }

    /// Get current buffer size in bytes
    pub fn buffer_size(&self) -> usize {
        let (lock, _) = &*self.state;
        if let Ok(state) = lock.lock() {
            state.len()
        } else {
            0
        }
    }

    /// Copy the buffered bytes into `out` in bounded chunks, re-taking the
    /// state lock per chunk instead of holding it for the whole transfer,
    /// so live readers are starved for at most one chunk's write instead of
    /// a full-track copy. Used by the low-memory oversized-track path to
    /// persist the finished track to the L2 disk cache straight from the
    /// playback buffer — no second full in-RAM copy. Returns bytes written.
    pub fn write_buffered_to<W: std::io::Write>(&self, out: &mut W) -> IoResult<usize> {
        if let Some(data) = self.complete_data_shared() {
            out.write_all(&data)?;
            return Ok(data.len());
        }
        let snapshot = {
            let state = self
                .state
                .0
                .lock()
                .map_err(|_| IoError::other("buffer lock poisoned"))?;
            if let Some(error) = state.download_error.as_ref() {
                return Err(IoError::other(error.clone()));
            }
            state.file.clone().map(|file| (file, state.len()))
        };
        if let Some((file, size)) = snapshot {
            let mut chunk = vec![0; 256 * 1024];
            let mut offset = 0;
            while offset < size {
                let count = chunk.len().min(size - offset);
                let n = file_read_at(&file, &mut chunk[..count], offset as u64)?;
                if n == 0 {
                    return Err(IoError::new(
                        ErrorKind::UnexpectedEof,
                        "stream spool truncated",
                    ));
                }
                out.write_all(&chunk[..n])?;
                offset += n;
            }
            return Ok(offset);
        }
        const CHUNK: usize = 1024 * 1024;
        let (lock, _) = &*self.state;
        let mut offset = 0usize;
        loop {
            let state = lock
                .lock()
                .map_err(|_| IoError::new(ErrorKind::Other, "Failed to acquire buffer lock"))?;
            if offset >= state.len() {
                break;
            }
            let end = (offset + CHUNK).min(state.len());
            out.write_all(&state.data[offset..end])?;
            offset = end;
        }
        Ok(offset)
    }
}

// =============================================================================
// IncrementalStreamingSource - A rodio Source that decodes on-demand
// =============================================================================

/// A rodio Source that decodes audio packets incrementally from a BufferedMediaSource.
///
/// This allows playback to start immediately after the initial buffer is filled,
/// while the rest of the file continues downloading in the background.
///
/// The source maintains an internal queue of decoded samples and decodes more
/// packets on-demand as samples are consumed.
pub struct IncrementalStreamingSource {
    /// Sample rate of the audio
    sample_rate: u32,
    /// Number of channels
    channels: u16,
    /// Queue of decoded samples ready to play
    sample_queue: VecDeque<f32>,
    /// The format reader (demuxer)
    format: Box<dyn FormatReader>,
    /// The audio decoder
    decoder: Box<dyn Decoder>,
    /// Track ID we're decoding
    track_id: u32,
    /// Whether we've reached end of stream
    finished: bool,
    /// Number of packets decoded (for stats)
    packets_decoded: u64,
    /// True while inside a WouldBlock stall episode (playback caught up
    /// with the download). Set on the first WouldBlock after at least one
    /// decoded packet, cleared on the next successful decode — so each
    /// episode records exactly one underrun with the network throttle.
    stalled: bool,
    /// Reference to the buffered source (for cache retrieval after playback)
    buffered_source: Arc<BufferedMediaSource>,
    /// Optional generation-safe side channel into the player's buffer state.
    buffer_reporter: Option<PlaybackBufferReporter>,
}

impl IncrementalStreamingSource {
    /// Create a new incremental streaming source.
    ///
    /// This initializes the symphonia decoder and prepares for incremental decoding.
    /// The BufferedMediaSource should already have its initial buffer filled.
    ///
    /// Returns the source along with detected sample_rate and channels.
    pub fn new(buffered_source: Arc<BufferedMediaSource>) -> Result<Self, String> {
        Self::new_inner(buffered_source, None)
    }

    pub(super) fn new_for_play(
        buffered_source: Arc<BufferedMediaSource>,
        buffer_reporter: PlaybackBufferReporter,
    ) -> Result<Self, String> {
        Self::new_inner(buffered_source, Some(buffer_reporter))
    }

    fn new_inner(
        buffered_source: Arc<BufferedMediaSource>,
        buffer_reporter: Option<PlaybackBufferReporter>,
    ) -> Result<Self, String> {
        // Create a reader from the buffered source
        let reader = buffered_source.create_reader();
        let media_source = Box::new(reader) as Box<dyn MediaSource>;
        let mss = MediaSourceStream::new(media_source, Default::default());

        let mut hint = Hint::new();
        hint.with_extension("flac"); // Most Qobuz Hi-Res is FLAC

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let metadata_opts: MetadataOptions = Default::default();

        let probed = get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|err| format!("Symphonia probe failed for streaming: {}", err))?;

        let track = probed
            .format
            .default_track()
            .ok_or_else(|| "Symphonia: no supported audio tracks in stream".to_string())?;

        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        // Extract sample rate and channels from codec params
        let metadata = super::audio_metadata_from_codec_params(&codec_params)?;
        let sample_rate = metadata.sample_rate;
        let channels = metadata.channels;

        let decoder = get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|err| format!("Symphonia decoder init failed for streaming: {}", err))?;

        log::info!(
            "IncrementalStreamingSource initialized: {}Hz, {} channels",
            sample_rate,
            channels
        );

        Ok(Self {
            sample_rate,
            channels,
            sample_queue: VecDeque::with_capacity(sample_rate as usize * channels as usize), // ~1s buffer
            format: probed.format,
            decoder,
            track_id,
            finished: false,
            packets_decoded: 0,
            stalled: false,
            buffered_source,
            buffer_reporter,
        })
    }

    /// Get the sample rate
    pub fn get_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get the number of channels
    pub fn get_channels(&self) -> u16 {
        self.channels
    }

    /// Get reference to the buffered source for cache retrieval
    pub fn buffered_source(&self) -> &Arc<BufferedMediaSource> {
        &self.buffered_source
    }

    /// Seek the decoder to the given time using Symphonia's native seek.
    ///
    /// For FLAC this uses the seek table to jump directly to the nearest
    /// seek point, then decodes forward to the exact sample — far cheaper
    /// than skip_duration's decode-every-sample-from-zero path. For MP3
    /// with Xing/VBRI headers it uses the TOC; without headers, Symphonia
    /// falls back to a binary search, still much cheaper than linear decode.
    ///
    /// The underlying BufferedMediaSource::seek is the I/O target. If the
    /// requested byte offset isn't buffered yet it will block on the
    /// condition variable — callers must only invoke this for times within
    /// the downloaded watermark.
    pub fn seek_to(&mut self, time: Duration) -> Result<(), String> {
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: time.into(),
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| format!("Symphonia seek failed: {}", e))?;
        self.decoder.reset();
        self.sample_queue.clear();
        self.packets_decoded = 0;
        self.finished = false;
        Ok(())
    }

    /// Decode more packets to fill the sample queue.
    ///
    /// This is called when the sample queue is running low.
    /// It will decode packets until the queue has at least `min_samples` or EOF is reached.
    fn decode_more(&mut self, min_samples: usize) {
        if self.finished {
            return;
        }

        while self.sample_queue.len() < min_samples {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    // Not enough data buffered yet - wait briefly and retry
                    // This happens when playback catches up with download
                    if !self.stalled && self.packets_decoded > 0 {
                        // Mid-playback stall, not initial buffering (≥1 packet
                        // already decoded): put the prefetch throttle in panic
                        // mode so the live stream gets the pipe to itself (#591).
                        self.stalled = true;
                        qbz_audio::network_throttle::state().record_underrun();
                        if let Some(reporter) = &self.buffer_reporter {
                            reporter.report(PlaybackBufferState::Underrun);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(SymphoniaError::IoError(_)) => {
                    // EOF or other IO error
                    log::info!(
                        "IncrementalStreamingSource: EOF reached after {} packets",
                        self.packets_decoded
                    );
                    self.finished = true;
                    if self.buffered_source.download_error().is_some() {
                        if let Some(reporter) = &self.buffer_reporter {
                            reporter.report(PlaybackBufferState::Error);
                        }
                    }
                    return;
                }
                Err(err) => {
                    log::error!("Symphonia read error in stream: {}", err);
                    self.finished = true;
                    if let Some(reporter) = &self.buffer_reporter {
                        reporter.report(PlaybackBufferState::Error);
                    }
                    return;
                }
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(audio_buf) => {
                    // Vorbis/Opus setup, comment, and padding packets can decode to zero frames.
                    // Passing one to copy_interleaved_ref panics on the CPAL output thread,
                    // tearing down audio output and closing its command channel for the session.
                    if audio_buf.frames() == 0 {
                        continue;
                    }
                    let spec = *audio_buf.spec();
                    let mut sample_buf = SampleBuffer::<f32>::new(audio_buf.frames() as u64, spec);
                    sample_buf.copy_interleaved_ref(audio_buf);

                    // Add samples to queue
                    self.sample_queue
                        .extend(sample_buf.samples().iter().copied());
                    let ready_edge = should_report_ready(self.packets_decoded, self.stalled);
                    self.packets_decoded += 1;
                    // Successful decode ends any stall episode; the next
                    // WouldBlock streak records a fresh underrun.
                    self.stalled = false;
                    if ready_edge {
                        if let Some(reporter) = &self.buffer_reporter {
                            reporter.report(PlaybackBufferState::Ready);
                        }
                    }
                }
                Err(SymphoniaError::DecodeError(e)) => {
                    log::warn!("Decode error (skipping packet): {}", e);
                    continue;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(err) => {
                    log::error!("Symphonia decode error: {}", err);
                    self.finished = true;
                    if let Some(reporter) = &self.buffer_reporter {
                        reporter.report(PlaybackBufferState::Error);
                    }
                    return;
                }
            }
        }
    }
}

const fn should_report_ready(packets_decoded: u64, stalled: bool) -> bool {
    packets_decoded == 0 || stalled
}

impl Source for IncrementalStreamingSource {
    fn current_span_len(&self) -> Option<usize> {
        // We don't know frame boundaries in the queue
        None
    }

    fn channels(&self) -> std::num::NonZero<u16> {
        std::num::NonZero::new(self.channels).unwrap()
    }

    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(self.sample_rate).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        // We don't know total duration until download completes
        // Could estimate from content-length if available
        None
    }
}

impl Iterator for IncrementalStreamingSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        // If queue is running low, decode more
        // Keep at least 0.5 seconds of audio buffered
        let min_buffer = (self.sample_rate as usize * self.channels as usize) / 2;
        if self.sample_queue.len() < min_buffer {
            self.decode_more(min_buffer);
        }

        self.sample_queue.pop_front()
    }
}

/// Shared, immutable audio bytes: lets several readers (the player, the cast
/// media server, the cast visualizer's shadow decoder) sit on ONE copy of a
/// 100 MB track instead of cloning it per consumer.
pub struct SharedBytes(pub Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Cursor-backed MediaSource for in-memory audio data.
struct InMemoryMediaSource {
    inner: Cursor<SharedBytes>,
    len: u64,
}

impl InMemoryMediaSource {
    fn shared(data: Arc<Vec<u8>>) -> Self {
        let len = data.len() as u64;
        Self {
            inner: Cursor::new(SharedBytes(data)),
            len,
        }
    }
}

impl Read for InMemoryMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        self.inner.read(buf)
    }
}

impl Seek for InMemoryMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> IoResult<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for InMemoryMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// Symphonia-backed decoder for fully-in-memory audio bytes, with native
/// seek support.
///
/// Exists because rodio's `skip_duration` decodes every sample from the
/// start of the track when seeking, which on FLAC Hi-Res costs several
/// seconds of CPU for long jumps and stalls the audio thread. This
/// source uses `FormatReader::seek(Accurate, SeekTo::Time)` — FLAC seek
/// table, MP3 Xing/VBRI TOC — to jump straight to the target sample, so
/// the post-seek decode window is ~O(seek point density) instead of
/// O(position).
///
/// Non-Symphonia formats (notably rodio's native MP4/AAC path) aren't
/// supported here; callers must fall back to `decode_with_fallback` +
/// `skip_duration` when `new` returns `Err`.
pub struct InMemorySource {
    sample_rate: u32,
    channels: u16,
    sample_queue: VecDeque<f32>,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    finished: bool,
}

impl InMemorySource {
    pub fn new(data: Vec<u8>) -> Result<Self, String> {
        Self::from_shared(Arc::new(data))
    }

    /// Decode bytes that another consumer keeps alive too (no copy).
    pub fn from_shared(data: Arc<Vec<u8>>) -> Result<Self, String> {
        let source = Box::new(InMemoryMediaSource::shared(data)) as Box<dyn MediaSource>;
        let mss = MediaSourceStream::new(source, Default::default());

        let hint = Hint::new();

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let metadata_opts: MetadataOptions = Default::default();

        let probed = get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|err| format!("Symphonia probe failed for in-memory source: {}", err))?;

        let track = probed
            .format
            .default_track()
            .ok_or_else(|| "Symphonia: no supported audio tracks".to_string())?;

        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        let metadata = super::audio_metadata_from_codec_params(&codec_params)?;
        let sample_rate = metadata.sample_rate;
        let channels = metadata.channels;

        let decoder = get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|err| format!("Symphonia decoder init failed: {}", err))?;

        Ok(Self {
            sample_rate,
            channels,
            sample_queue: VecDeque::with_capacity(sample_rate as usize * channels as usize),
            format: probed.format,
            decoder,
            track_id,
            finished: false,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn seek_to(&mut self, time: Duration) -> Result<(), String> {
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: time.into(),
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| format!("Symphonia in-memory seek failed: {}", e))?;
        self.decoder.reset();
        self.sample_queue.clear();
        self.finished = false;
        Ok(())
    }

    fn decode_more(&mut self, min_samples: usize) {
        if self.finished {
            return;
        }

        while self.sample_queue.len() < min_samples {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(_)) => {
                    self.finished = true;
                    return;
                }
                Err(err) => {
                    log::error!("Symphonia read error in in-memory source: {}", err);
                    self.finished = true;
                    return;
                }
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(audio_buf) => {
                    // Vorbis/Opus setup, comment, and padding packets can decode to zero frames.
                    // Passing one to copy_interleaved_ref panics on the CPAL output thread,
                    // tearing down audio output and closing its command channel for the session.
                    if audio_buf.frames() == 0 {
                        continue;
                    }
                    let spec = *audio_buf.spec();
                    let mut sample_buf = SampleBuffer::<f32>::new(audio_buf.frames() as u64, spec);
                    sample_buf.copy_interleaved_ref(audio_buf);
                    self.sample_queue
                        .extend(sample_buf.samples().iter().copied());
                }
                Err(SymphoniaError::DecodeError(e)) => {
                    log::warn!("Decode error (skipping packet): {}", e);
                    continue;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(err) => {
                    log::error!("Symphonia decode error: {}", err);
                    self.finished = true;
                    return;
                }
            }
        }
    }
}

impl Source for InMemorySource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> std::num::NonZero<u16> {
        std::num::NonZero::new(self.channels).unwrap()
    }

    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(self.sample_rate).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

impl Iterator for InMemorySource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let min_buffer = (self.sample_rate as usize * self.channels as usize) / 2;
        if self.sample_queue.len() < min_buffer {
            self.decode_more(min_buffer);
        }
        self.sample_queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn hires_wav() -> Vec<u8> {
        // A real PCM WAV container: stereo, signed 24-bit, 192 kHz, two seconds.
        let frames = 384000u32;
        let size = frames * 6;
        let mut wav = Vec::with_capacity(size as usize + 44);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(size + 36).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&192000u32.to_le_bytes());
        wav.extend_from_slice(&1152000u32.to_le_bytes());
        wav.extend_from_slice(&6u16.to_le_bytes());
        wav.extend_from_slice(&24u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&size.to_le_bytes());
        for frame in 0..frames {
            let value = (frame % 100000) as i32 - 50000;
            for sample in [value, -value] {
                wav.extend_from_slice(&sample.to_le_bytes()[..3]);
            }
        }
        wav
    }

    #[test]
    fn disk_24bit_192khz_decode_and_seek_match_memory_source() {
        let wav = hires_wav();
        let (source, writer) = BufferedMediaSource::new_on_disk(
            StreamingConfig::fast_start(),
            Some(wav.len() as u64),
            tempfile::tempfile().unwrap(),
        );
        writer.push_chunk(&wav[..wav.len() / 2]).unwrap();
        let source = Arc::new(source);
        let mut disk = IncrementalStreamingSource::new(source.clone()).unwrap();
        let mut memory = InMemorySource::new(wav.clone()).unwrap();
        assert_eq!(disk.sample_rate().get(), 192000);
        assert_eq!(disk.channels().get(), 2);
        for _ in 0..1000 {
            assert_eq!(disk.next(), memory.next());
        }
        writer.push_chunk(&wav[wav.len() / 2..]).unwrap();
        writer.complete().unwrap();
        for _ in 1000..384000 * 2 {
            assert_eq!(disk.next(), memory.next());
        }
        assert_eq!(disk.next(), None);
        for seconds in [1, 0, 1] {
            disk.seek_to(Duration::from_secs(seconds)).unwrap();
            memory.seek_to(Duration::from_secs(seconds)).unwrap();
            for _ in 0..1000 {
                assert_eq!(disk.next(), memory.next());
            }
        }
        assert_eq!(source.state.0.lock().unwrap().data.capacity(), 0);
    }

    #[test]
    fn real_disk_write_failure_is_published_to_readers() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let file = File::open(temp.path()).unwrap(); // read-only descriptor
        let (mut source, writer) =
            BufferedMediaSource::new_on_disk(StreamingConfig::fast_start(), None, file);
        assert!(writer
            .push_chunk(b"cannot write")
            .unwrap_err()
            .contains("spool write failed"));
        assert!(source
            .download_error()
            .unwrap()
            .contains("spool write failed"));
        assert!(source.read(&mut [0; 1]).is_err());
    }

    #[test]
    fn disk_buffer_streams_concurrently_without_retaining_payload_in_ram() {
        let temp = tempfile::tempfile().unwrap();
        let count = 256;
        let chunk = vec![73; 16 * 1024];
        let size = count * chunk.len();
        let (mut source, writer) = BufferedMediaSource::new_on_disk(
            StreamingConfig::fast_start(),
            Some(size as u64),
            temp,
        );
        let feeder = thread::spawn(move || {
            for _ in 0..count {
                writer.push_chunk(&chunk).unwrap();
            }
            writer.complete().unwrap();
            assert!(writer.push_chunk(b"late").is_err());
            writer
        });
        let mut consumed = 0;
        let mut read = [0; 4096];
        loop {
            let n = source.read(&mut read).unwrap();
            if n == 0 {
                break;
            }
            assert!(read[..n].iter().all(|byte| *byte == 73));
            consumed += n;
        }
        let writer = feeder.join().unwrap();
        assert_eq!(consumed, size);
        assert_eq!(source.state.0.lock().unwrap().data.capacity(), 0);
        assert!(source.complete_data_shared().is_none());
        let mut reader2 = source.create_reader();
        source.seek(SeekFrom::Start(123)).unwrap();
        source.read_exact(&mut read[..1]).unwrap();
        assert_eq!(
            reader2.read_pos.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        reader2.seek(SeekFrom::End(-1)).unwrap();
        reader2.read_exact(&mut read[..1]).unwrap();
        assert_eq!(read[0], 73);
        let mut copied = Vec::new();
        assert_eq!(writer.write_buffered_to(&mut copied).unwrap(), size);
        assert!(copied.iter().all(|byte| *byte == 73));
    }

    #[test]
    fn release_interrupts_a_waiting_reader_without_sealing_download() {
        let (source, writer) = BufferedMediaSource::new(StreamingConfig::fast_start(), None);
        let source = Arc::new(source);
        let mut reader = source.create_reader();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || { done_tx.send(reader.read(&mut [0; 8])).unwrap(); });
        source.interrupt_readers();
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap(), 0);
        worker.join().unwrap();
        assert!(!source.is_complete());
        writer.push_chunk(b"resume").unwrap();
        let mut resumed = source.create_reader();
        let mut bytes = [0; 6];
        resumed.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"resume");
    }

    #[test]
    fn failed_disk_buffer_wakes_reader_and_cannot_be_cached() {
        let (mut source, writer) = BufferedMediaSource::new_on_disk(
            StreamingConfig::fast_start(),
            None,
            tempfile::tempfile().unwrap(),
        );
        let reader = thread::spawn(move || source.read(&mut [0; 8]));
        writer.error("fixture disk failure".into()).unwrap();
        assert!(reader.join().unwrap().is_err());
        assert!(writer.write_buffered_to(&mut Vec::new()).is_err());
        assert!(writer.push_chunk(b"late").is_err());
    }

    #[test]
    fn sealed_buffer_shares_allocation_without_truncating_active_reader() {
        let (mut source, writer) =
            BufferedMediaSource::new(StreamingConfig::from_seconds(1), Some(10));
        writer.push_chunk(b"0123456789").unwrap();
        assert!(source.complete_data_shared().is_none());
        let pointer = source.state.0.lock().unwrap().data.as_ptr();
        let mut prefix = [0; 3];
        source.read_exact(&mut prefix).unwrap();
        writer.complete().unwrap();
        let shared = source.complete_data_shared().unwrap();
        assert_eq!(pointer, shared.as_ptr());
        assert!(Arc::ptr_eq(
            &shared,
            &writer.complete_data_shared().unwrap()
        ));
        assert!(writer.push_chunk(b"late bytes").is_err());
        let mut tail = Vec::new();
        source.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, b"3456789");
        source.seek(SeekFrom::Start(0)).unwrap();
        source.read_exact(&mut prefix).unwrap();
        assert_eq!(prefix, *b"012");
    }

    #[test]
    fn promotion_keeps_24bit_192khz_samples_continuous_and_seekable() {
        let frames = 384000u32;
        let size = frames * 6;
        let wav = hires_wav();
        let (buffer, writer) =
            BufferedMediaSource::new(StreamingConfig::from_seconds(1), Some(wav.len() as u64));
        let split = 44 + size as usize / 2;
        writer.push_chunk(&wav[..split]).unwrap();
        let buffer = Arc::new(buffer);
        let mut streaming = IncrementalStreamingSource::new(buffer.clone()).unwrap();
        assert_eq!(streaming.get_sample_rate(), 192000);
        let expected = |index: usize| {
            let value = ((index / 2) % 100000) as i32 - 50000;
            (if index % 2 == 0 { value } else { -value }) as f32 / 8388608.0
        };
        for index in 0..1000 {
            assert_eq!(streaming.next().unwrap(), expected(index));
        }
        writer.push_chunk(&wav[split..]).unwrap();
        writer.complete().unwrap();
        let shared = buffer.complete_data_shared().unwrap();
        for index in 1000..frames as usize * 2 {
            assert_eq!(streaming.next().unwrap(), expected(index));
        }
        assert!(streaming.next().is_none());
        let mut replay = InMemorySource::from_shared(shared).unwrap();
        assert_eq!(replay.sample_rate(), 192000);
        assert_eq!(replay.channels(), 2);
        // Seek remains packet-aligned, matching the pre-existing Vec-backed path.
        let mut baseline = InMemorySource::new(wav).unwrap();
        baseline.seek_to(Duration::from_secs(1)).unwrap();
        replay.seek_to(Duration::from_secs(1)).unwrap();
        for _ in 0..1000 {
            assert_eq!(replay.next().unwrap(), baseline.next().unwrap());
        }
        replay.seek_to(Duration::ZERO).unwrap();
        for index in 0..1000 {
            assert_eq!(replay.next().unwrap(), expected(index));
        }
    }

    #[test]
    #[ignore = "requires QBZ_HIRES_FIXTURE pointing to a real 24/192 FLAC file"]
    fn real_hires_file_streaming_promotion_matches_uninterrupted_decode() {
        let path = std::env::var("QBZ_HIRES_FIXTURE").expect("set QBZ_HIRES_FIXTURE");
        let bytes = std::fs::read(path).unwrap();
        let (buffer, writer) =
            BufferedMediaSource::new(StreamingConfig::from_seconds(1), Some(bytes.len() as u64));
        writer.push_chunk(&bytes).unwrap();
        drop(bytes);
        let buffer = Arc::new(buffer);
        let mut streaming = IncrementalStreamingSource::new(buffer.clone()).unwrap();
        assert_eq!(streaming.get_sample_rate(), 192000);
        let first: Vec<_> = streaming.by_ref().take(1000).collect();
        writer.complete().unwrap();
        let shared = buffer.complete_data_shared().unwrap();
        let mut reference = InMemorySource::from_shared(shared).unwrap();
        assert_eq!(reference.sample_rate(), 192000);
        for sample in first {
            assert_eq!(Some(sample), reference.next());
        }
        let mut count = 1000usize;
        for sample in streaming {
            assert_eq!(Some(sample), reference.next());
            count += 1;
        }
        assert!(reference.next().is_none());
        assert!(count > 384000);
    }

    #[test]
    #[ignore = "requires QBZ_HIRES_FIXTURE pointing to a real 24/192 FLAC file"]
    fn real_hires_disk_file_matches_memory_decode_and_seeks() {
        let path = std::env::var("QBZ_HIRES_FIXTURE").expect("set QBZ_HIRES_FIXTURE");
        let buffer = Arc::new(BufferedMediaSource::from_file(std::fs::File::open(&path).unwrap()).unwrap());
        let mut disk = IncrementalStreamingSource::new(buffer.clone()).unwrap();
        let mut reference = InMemorySource::new(std::fs::read(path).unwrap()).unwrap();
        assert_eq!(disk.get_sample_rate(), 192000);
        assert_eq!(reference.sample_rate(), 192000);
        let mut count = 0usize;
        for sample in disk.by_ref() {
            assert_eq!(Some(sample), reference.next(), "sample {count}");
            count += 1;
        }
        assert!(reference.next().is_none());
        assert!(count > 384000);
        for seconds in [500, 10, 0, 600] {
            let position = Duration::from_secs(seconds);
            disk.seek_to(position).unwrap();
            reference.seek_to(position).unwrap();
            for index in 0..384000 {
                assert_eq!(disk.next(), reference.next(), "seek {seconds}s sample {index}");
            }
        }
        assert!(buffer.complete_data_shared().is_none());
        assert_eq!(buffer.state.0.lock().unwrap().data.capacity(), 0);
        eprintln!("real disk FLAC: {count} samples matched, four seeks matched");
    }

    #[test]
    fn ready_reports_only_on_first_decode_and_stall_recovery() {
        assert!(should_report_ready(0, false));
        assert!(!should_report_ready(1, false));
        assert!(!should_report_ready(400, false));
        assert!(should_report_ready(1, true));
        assert!(should_report_ready(400, true));
    }

    #[test]
    fn first_error_wins_over_later_generic_abort() {
        let (source, writer) = BufferedMediaSource::new(StreamingConfig::from_seconds(1), None);
        writer.error("root cause".to_string()).unwrap();
        writer
            .error("CMAF stream aborted before completion".to_string())
            .unwrap();
        assert_eq!(source.download_error().as_deref(), Some("root cause"));
    }

    #[test]
    fn feeder_error_is_visible_to_waiters_before_min_buffer() {
        let (source, writer) = BufferedMediaSource::new(StreamingConfig::from_seconds(1), None);
        assert!(!source.has_min_buffer());
        assert!(source.download_error().is_none());
        writer.error("feeder died".to_string()).unwrap();
        // The initial-buffer wait loop polls this instead of sleeping out
        // the full buffer timeout.
        assert_eq!(source.download_error().as_deref(), Some("feeder died"));
        assert!(!source.has_min_buffer());
    }

    #[test]
    fn raw_initial_buffer_for_speed_follows_documented_ladder() {
        // Each band of the documented speed ladder produces its own size.
        assert_eq!(raw_initial_buffer_for_speed(20.0), 256 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(10.0), 256 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(7.0), 384 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(5.0), 384 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(3.0), 512 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(2.0), 512 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(1.5), 1024 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(1.0), 1024 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(0.5), 2 * 1024 * 1024);
        assert_eq!(raw_initial_buffer_for_speed(0.0), 2 * 1024 * 1024);
    }

    #[test]
    fn from_speed_mbps_with_cap_passes_through_when_under_cap() {
        // Cap above the raw value: result equals the raw ladder.
        let cfg = StreamingConfig::from_speed_mbps_with_cap(0.0, 4 * 1024 * 1024);
        assert_eq!(cfg.initial_buffer_bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn from_speed_mbps_with_cap_clamps_slow_connection_to_low_memory_cap() {
        // The case from issue #331: Pi 3B, slow connection because of swap
        // thrash, would otherwise inflate the buffer to 2 MB. With the
        // LowMemory profile's 256KB cap applied, we stay at 256KB.
        let cfg = StreamingConfig::from_speed_mbps_with_cap(0.0, 256 * 1024);
        assert_eq!(cfg.initial_buffer_bytes, 256 * 1024);

        let cfg = StreamingConfig::from_speed_mbps_with_cap(1.5, 256 * 1024);
        assert_eq!(cfg.initial_buffer_bytes, 256 * 1024);
    }

    #[test]
    fn from_speed_mbps_with_cap_no_op_for_normal_profile() {
        // Normal profile cap is 2 MB — equal to the slowest raw band, so
        // any raw value passes through unchanged.
        let cap = 2 * 1024 * 1024;
        for speed in [0.0, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0] {
            let cfg = StreamingConfig::from_speed_mbps_with_cap(speed, cap);
            assert_eq!(
                cfg.initial_buffer_bytes,
                raw_initial_buffer_for_speed(speed),
                "cap should not bind for speed={}",
                speed
            );
        }
    }

    #[test]
    fn from_speed_mbps_with_cap_max_buffer_unchanged() {
        // Whatever the cap, the secondary max_buffer_bytes stays at its
        // module default; we are only clamping the initial fill target.
        let cfg = StreamingConfig::from_speed_mbps_with_cap(0.5, 64 * 1024);
        assert_eq!(cfg.max_buffer_bytes, 100 * 1024 * 1024);
    }

    #[test]
    fn prealloc_capacity_decision() {
        // Unknown or zero size: no reservation, caller uses the initial
        // buffer capacity instead.
        assert_eq!(prealloc_capacity(None), None);
        assert_eq!(prealloc_capacity(Some(0)), None);
        // Known size reserves exactly that (no doubling slack).
        assert_eq!(
            prealloc_capacity(Some(60 * 1024 * 1024)),
            Some(60 * 1024 * 1024)
        );
        assert_eq!(prealloc_capacity(Some(1)), Some(1));
        // Absurd sizes are capped at 1 GiB.
        assert_eq!(
            prealloc_capacity(Some(8 * 1024 * 1024 * 1024)),
            Some(MAX_PREALLOC_BYTES as usize)
        );
    }

    #[test]
    fn new_source_preallocates_known_total_size() {
        let config = StreamingConfig {
            initial_buffer_bytes: 16,
            max_buffer_bytes: 100,
        };
        let (_source, writer) = BufferedMediaSource::new(config, Some(4096));
        // The writer's buffer should have reserved the full track up front.
        // (No public capacity accessor — push the full size and ensure the
        // data lands intact, which is the observable contract.)
        let payload = vec![0xABu8; 4096];
        writer.push_chunk(&payload).unwrap();
        writer.complete().unwrap();
        assert_eq!(writer.buffer_size(), 4096);
    }

    #[test]
    fn write_buffered_to_copies_full_contents() {
        let config = StreamingConfig {
            initial_buffer_bytes: 4,
            max_buffer_bytes: 100,
        };
        let (_source, writer) = BufferedMediaSource::new(config, None);
        writer.push_chunk(b"Hello, ").unwrap();
        writer.push_chunk(b"world!").unwrap();
        writer.complete().unwrap();

        let mut out: Vec<u8> = Vec::new();
        let written = writer.write_buffered_to(&mut out).unwrap();
        assert_eq!(written, 13);
        assert_eq!(&out, b"Hello, world!");
    }

    #[test]
    fn write_buffered_to_handles_empty_buffer() {
        let config = StreamingConfig {
            initial_buffer_bytes: 4,
            max_buffer_bytes: 100,
        };
        let (_source, writer) = BufferedMediaSource::new(config, None);
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(writer.write_buffered_to(&mut out).unwrap(), 0);
        assert!(out.is_empty());
    }

    #[test]
    fn test_basic_read_write() {
        let config = StreamingConfig {
            initial_buffer_bytes: 10,
            max_buffer_bytes: 100,
        };
        let (mut source, writer) = BufferedMediaSource::new(config, Some(20));

        // Write some data
        writer.push_chunk(b"Hello").unwrap();
        writer.push_chunk(b"World").unwrap();

        // Read it back
        let mut buf = [0u8; 5];
        assert_eq!(source.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf, b"Hello");

        assert_eq!(source.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf, b"World");
    }

    #[test]
    fn test_seek_within_buffer() {
        let config = StreamingConfig {
            initial_buffer_bytes: 5,
            max_buffer_bytes: 100,
        };
        let (mut source, writer) = BufferedMediaSource::new(config, Some(10));

        writer.push_chunk(b"0123456789").unwrap();
        writer.complete().unwrap();

        // Read first 5 bytes
        let mut buf = [0u8; 5];
        source.read(&mut buf).unwrap();
        assert_eq!(&buf, b"01234");

        // Seek back to start
        source.seek(SeekFrom::Start(0)).unwrap();
        source.read(&mut buf).unwrap();
        assert_eq!(&buf, b"01234");

        // Seek to middle
        source.seek(SeekFrom::Start(3)).unwrap();
        source.read(&mut buf).unwrap();
        assert_eq!(&buf, b"34567");
    }

    #[test]
    fn test_complete_data_retrieval() {
        let config = StreamingConfig {
            initial_buffer_bytes: 5,
            max_buffer_bytes: 100,
        };
        let (source, writer) = BufferedMediaSource::new(config, Some(10));

        writer.push_chunk(b"Hello").unwrap();
        assert!(source.take_complete_data().is_none()); // Not complete yet

        writer.push_chunk(b"World").unwrap();
        writer.complete().unwrap();

        let data = source.take_complete_data().unwrap();
        assert_eq!(&data, b"HelloWorld");
    }

    #[test]
    fn test_blocking_read() {
        let config = StreamingConfig {
            initial_buffer_bytes: 5,
            max_buffer_bytes: 100,
        };
        let (mut source, writer) = BufferedMediaSource::new(config, None);

        // Spawn thread to write after delay
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            writer.push_chunk(b"Delayed").unwrap();
            writer.complete().unwrap();
        });

        // This should block until data arrives
        let mut buf = [0u8; 7];
        let n = source.read(&mut buf).unwrap();
        assert_eq!(n, 7);
        assert_eq!(&buf, b"Delayed");
    }

    #[test]
    fn error_unblocks_reader_with_io_error() {
        use std::io::ErrorKind;
        let config = StreamingConfig {
            initial_buffer_bytes: 5,
            max_buffer_bytes: 100,
        };
        let (mut source, writer) = BufferedMediaSource::new(config, None);
        writer.error("cdn failed".into()).unwrap();
        let mut buf = [0u8; 8];
        let err = source.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
        let msg = err.to_string();
        assert!(msg.contains("cdn failed"), "{msg}");
    }
}
