//! Playback Engine Abstraction
//!
//! Unified interface for different playback backends:
//! - Rodio (PipeWire, Pulse, ALSA via CPAL) - uses rodio::Sink
//! - ALSA Direct (hw: devices) - bypasses rodio, writes directly to ALSA PCM
//!
//! ALSA Direct uses a single long-lived writer thread with a source queue
//! to enable gapless playback. When one source ends, the next is picked up
//! seamlessly without interrupting the PCM stream.

use qbz_audio::alsa_hardware_volume::{
    read_hardware_volume, subscribe_hardware_volume_events, AlsaMixerControlId,
    HardwareVolumeEvent, HardwareVolumeEventCallback, HardwareVolumeEventSubscription,
};
#[cfg(target_os = "linux")]
use qbz_audio::AlsaDirectStream;
use qbz_audio::VisualizerTap;
#[cfg(target_os = "linux")]
use qbz_audio::JackStream;
use rodio::{mixer::Mixer, Player as RodioPlayer, Source};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

/// A boxed sample iterator that can be sent across threads
type BoxedSampleIter = Box<dyn Iterator<Item = f32> + Send>;

/// A boxed DoP word iterator (pre-packed S32 DoP samples — see qbz-dsd)
#[cfg(target_os = "linux")]
type BoxedDopIter = Box<dyn Iterator<Item = i32> + Send>;

#[cfg(target_os = "linux")]
type DopReplacement = (BoxedDopIter, u64);

/// Thread-safe source queue for gapless playback.
/// The writer thread consumes sources; append() pushes new ones.
pub(crate) struct SourceQueue<S> {
    queue: Mutex<VecDeque<S>>,
    /// Notifies the writer thread that a new source is available
    notify: Condvar,
}

impl<S> SourceQueue<S> {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            notify: Condvar::new(),
        }
    }

    /// Push a new source to the back of the queue
    fn push(&self, source: S) {
        let mut q = self.queue.lock().unwrap();
        q.push_back(source);
        self.notify.notify_one();
    }

    /// Try to pop the next source (non-blocking)
    fn try_pop(&self) -> Option<S> {
        let mut q = self.queue.lock().unwrap();
        q.pop_front()
    }

    /// Wait for a source to become available (with timeout)
    /// Returns None on timeout (used to check stop/pause flags)
    fn wait_for_source(&self, timeout: Duration) -> Option<S> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() {
            let (guard, _) = self.notify.wait_timeout(q, timeout).unwrap();
            q = guard;
        }
        q.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.queue.lock().unwrap().is_empty()
    }

    fn clear(&self) {
        self.queue.lock().unwrap().clear();
    }

    fn wake(&self) {
        self.notify.notify_one();
    }
}

/// Unified playback engine
pub enum PlaybackEngine {
    /// Rodio-based (PipeWire, Pulse, ALSA via CPAL)
    Rodio { sink: RodioPlayer },
    /// A bit-perfect DIRECT sink with a gapless source queue: ALSA hw: on
    /// Linux, WASAPI exclusive on Windows. The two are interchangeable here -
    /// the writer thread only ever calls `DirectSink`.
    Direct {
        stream: Arc<dyn qbz_audio::backend::DirectSink>,
        /// Log prefix for THIS sink. Without it `play`/`pause`/`stop`/
        /// `set_volume` printed "ALSA Direct Engine" for a WASAPI stream.
        label: &'static str,
        is_playing: Arc<AtomicBool>,
        should_stop: Arc<AtomicBool>,
        position_frames: Arc<AtomicU64>,
        duration_frames: Arc<AtomicU64>,
        source_queue: Arc<SourceQueue<BoxedSampleIter>>,
        playback_thread: Option<thread::JoinHandle<()>>,
        /// Signals that the writer thread has consumed a source and moved to next
        source_transition: Arc<AtomicBool>,
        hardware_volume_control: Option<AlsaMixerControlId>,
        hardware_volume_active: Arc<AtomicBool>,
        hardware_volume_events: HardwareVolumeEventCallback,
        _hardware_volume_subscription: Option<HardwareVolumeEventSubscription>,
    },
    /// Native JACK output (#263 Tier 3). Mirrors AlsaDirect (gapless source queue
    /// + a single long-lived feeder thread), but the feeder resamples each source
    /// to the JACK graph rate and writes interleaved stereo f32 into the client's
    /// lock-free ring buffer via `JackStream::write_f32`. NOT bit-perfect.
    #[cfg(target_os = "linux")]
    Jack {
        is_playing: Arc<AtomicBool>,
        should_stop: Arc<AtomicBool>,
        position_frames: Arc<AtomicU64>,
        duration_frames: Arc<AtomicU64>,
        source_queue: Arc<SourceQueue<BoxedSampleIter>>,
        feeder_thread: Option<thread::JoinHandle<()>>,
        source_transition: Arc<AtomicBool>,
        graph_rate: u32,
    },
    /// DoP (DSD over PCM) direct output (DSD plan Phase 2). Mirrors
    /// AlsaDirect's writer-thread + source-queue shape but carries
    /// pre-packed S32 DoP words written VERBATIM (no f32, no gain — one
    /// altered sample breaks the DoP markers and the DAC plays the raw
    /// bitstream as loud noise). Pause feeds 0x69 DSD silence so the DAC
    /// stays locked in DSD mode; the queue gives gapless DSD.
    #[cfg(target_os = "linux")]
    AlsaDop {
        stream: Arc<AlsaDirectStream>,
        is_playing: Arc<AtomicBool>,
        should_stop: Arc<AtomicBool>,
        position_frames: Arc<AtomicU64>,
        source_queue: Arc<SourceQueue<BoxedDopIter>>,
        /// A seek replaces the live source without closing ALSA. The writer
        /// inserts DSD silence before consuming it so the DAC stays locked.
        replacement: Arc<Mutex<Option<DopReplacement>>>,
        writer_thread: Option<thread::JoinHandle<()>>,
        source_transition: Arc<AtomicBool>,
    },
}

impl PlaybackEngine {
    /// Create Rodio engine
    pub fn new_rodio(mixer: &Mixer) -> Result<Self, String> {
        let sink = RodioPlayer::connect_new(mixer);
        Ok(Self::Rodio { sink })
    }

    /// Create a bit-perfect direct engine with a gapless source queue.
    /// Spawns a single writer thread that lives for the engine's lifetime.
    /// Takes ANY direct sink. The log prefix comes from the SINK, so no call
    /// site can mislabel it.
    pub fn new_direct(
        stream: Arc<dyn qbz_audio::backend::DirectSink>,
        hardware_volume_control: Option<AlsaMixerControlId>,
        visualizer_tap: Option<VisualizerTap>,
        hardware_volume_events: HardwareVolumeEventCallback,
    ) -> Self {
        let label = stream.log_label();
        let is_playing = Arc::new(AtomicBool::new(false));
        let should_stop = Arc::new(AtomicBool::new(false));
        let position_frames = Arc::new(AtomicU64::new(0));
        let duration_frames = Arc::new(AtomicU64::new(0));
        let source_queue = Arc::new(SourceQueue::new());
        let source_transition = Arc::new(AtomicBool::new(false));
        let hardware_volume_active = Arc::new(AtomicBool::new(hardware_volume_control.is_some()));

        let hardware_volume_subscription = hardware_volume_control.as_ref().and_then(|control| {
            let Some(device_id) = stream.alsa_device_id() else {
                hardware_volume_active.store(false, Ordering::SeqCst);
                hardware_volume_events(HardwareVolumeEvent::Unavailable(
                    "selected hardware volume has no ALSA device route".to_string(),
                ));
                return None;
            };
            match read_hardware_volume(device_id, control) {
                Ok(snapshot) => hardware_volume_events(HardwareVolumeEvent::Changed(snapshot)),
                Err(error) => {
                    hardware_volume_active.store(false, Ordering::SeqCst);
                    hardware_volume_events(HardwareVolumeEvent::Unavailable(error));
                    return None;
                }
            }
            let active = hardware_volume_active.clone();
            let forward = hardware_volume_events.clone();
            let callback: HardwareVolumeEventCallback = Arc::new(move |event| match event {
                HardwareVolumeEvent::Unavailable(error) => {
                    active.store(false, Ordering::SeqCst);
                    forward(HardwareVolumeEvent::Unavailable(error));
                }
                HardwareVolumeEvent::Changed(snapshot) if active.load(Ordering::SeqCst) => {
                    forward(HardwareVolumeEvent::Changed(snapshot));
                }
                HardwareVolumeEvent::Changed(_) => {}
            });
            match subscribe_hardware_volume_events(device_id, control, callback) {
                Ok(subscription) => Some(subscription),
                Err(error) => {
                    hardware_volume_active.store(false, Ordering::SeqCst);
                    hardware_volume_events(HardwareVolumeEvent::Unavailable(error));
                    None
                }
            }
        });

        // Spawn the single long-lived writer thread
        let handle = {
            let stream_c = stream.clone();
            let playing_c = is_playing.clone();
            let stop_c = should_stop.clone();
            let pos_c = position_frames.clone();
            let dur_c = duration_frames.clone();
            let queue_c = source_queue.clone();
            let transition_c = source_transition.clone();
            let channels = stream.channels();
            let visualizer_c = visualizer_tap.clone();

            thread::spawn(move || {
                direct_writer_thread(
                    stream_c,
                    playing_c,
                    stop_c,
                    pos_c,
                    dur_c,
                    queue_c,
                    transition_c,
                    channels,
                    label,
                    visualizer_c,
                );
            })
        };

        Self::Direct {
            stream,
            label,
            is_playing,
            should_stop,
            position_frames,
            duration_frames,
            source_queue,
            playback_thread: Some(handle),
            source_transition,
            hardware_volume_control,
            hardware_volume_active,
            hardware_volume_events,
            _hardware_volume_subscription: hardware_volume_subscription,
        }
    }

    /// Create a JACK engine with a gapless source queue (#263 Tier 3). Spawns one
    /// long-lived feeder thread that resamples each source to the JACK graph rate
    /// and writes it to the client's ring buffer.
    #[cfg(target_os = "linux")]
    pub fn new_jack(stream: Arc<JackStream>) -> Self {
        let is_playing = Arc::new(AtomicBool::new(false));
        let should_stop = Arc::new(AtomicBool::new(false));
        let position_frames = Arc::new(AtomicU64::new(0));
        let duration_frames = Arc::new(AtomicU64::new(0));
        let source_queue = Arc::new(SourceQueue::new());
        let source_transition = Arc::new(AtomicBool::new(false));
        let graph_rate = stream.sample_rate();

        let handle = {
            let stream_c = stream.clone();
            let playing_c = is_playing.clone();
            let stop_c = should_stop.clone();
            let pos_c = position_frames.clone();
            let dur_c = duration_frames.clone();
            let queue_c = source_queue.clone();
            let transition_c = source_transition.clone();
            thread::spawn(move || {
                jack_feeder_thread(
                    stream_c, playing_c, stop_c, pos_c, dur_c, queue_c, transition_c,
                );
            })
        };

        Self::Jack {
            is_playing,
            should_stop,
            position_frames,
            duration_frames,
            source_queue,
            feeder_thread: Some(handle),
            source_transition,
            graph_rate,
        }
    }

    /// Create a DoP engine over an S32 ALSA direct stream created with
    /// `AlsaDirectStream::new_dop`. Sources are queued via [`Self::append_dop`].
    #[cfg(target_os = "linux")]
    pub fn new_alsa_dop(stream: Arc<AlsaDirectStream>, native: bool) -> Self {
        let is_playing = Arc::new(AtomicBool::new(false));
        let should_stop = Arc::new(AtomicBool::new(false));
        let position_frames = Arc::new(AtomicU64::new(0));
        let source_queue: Arc<SourceQueue<BoxedDopIter>> = Arc::new(SourceQueue::new());
        let replacement: Arc<Mutex<Option<DopReplacement>>> = Arc::new(Mutex::new(None));
        let source_transition = Arc::new(AtomicBool::new(false));
        let handle = {
            let stream_c = stream.clone();
            let playing_c = is_playing.clone();
            let stop_c = should_stop.clone();
            let pos_c = position_frames.clone();
            let queue_c = source_queue.clone();
            let replacement_c = replacement.clone();
            let transition_c = source_transition.clone();
            let channels = stream.channels();
            thread::spawn(move || {
                dop_writer_thread(
                    stream_c,
                    playing_c,
                    stop_c,
                    pos_c,
                    queue_c,
                    replacement_c,
                    transition_c,
                    channels,
                    native,
                );
            })
        };
        Self::AlsaDop {
            stream,
            is_playing,
            should_stop,
            position_frames,
            source_queue,
            replacement,
            writer_thread: Some(handle),
            source_transition,
        }
    }

    /// Queue a DoP word source (gapless when one is already playing).
    #[cfg(target_os = "linux")]
    pub fn append_dop(&mut self, source: BoxedDopIter) -> Result<(), String> {
        match self {
            Self::AlsaDop {
                is_playing,
                should_stop,
                position_frames,
                source_queue,
                source_transition,
                ..
            } => {
                let is_first = source_queue.is_empty() && !is_playing.load(Ordering::SeqCst);
                source_queue.push(source);
                if is_first {
                    position_frames.store(0, Ordering::SeqCst);
                    should_stop.store(false, Ordering::SeqCst);
                    source_transition.store(false, Ordering::SeqCst);
                    is_playing.store(true, Ordering::SeqCst);
                    log::info!("[DoP Engine] First source queued, playback starting");
                } else {
                    log::info!("[DoP Engine] Source queued for gapless DSD transition");
                }
                Ok(())
            }
            _ => Err("append_dop on a non-DoP engine".to_string()),
        }
    }

    /// Replace the currently playing direct-DSD source for seek while keeping
    /// the exclusive ALSA stream open. Queued gapless successors are dropped;
    /// the audio thread will request them again near the new end position.
    #[cfg(target_os = "linux")]
    pub fn replace_dop(
        &mut self,
        source: BoxedDopIter,
        position_frames_base: u64,
    ) -> Result<(), String> {
        match self {
            Self::AlsaDop {
                should_stop,
                source_queue,
                replacement,
                source_transition,
                ..
            } => {
                if should_stop.load(Ordering::SeqCst) {
                    return Err("direct DSD writer is stopped".to_string());
                }
                source_queue.clear();
                source_transition.store(false, Ordering::SeqCst);
                *replacement.lock().unwrap() = Some((source, position_frames_base));
                source_queue.wake();
                Ok(())
            }
            _ => Err("replace_dop on a non-DoP engine".to_string()),
        }
    }

    /// True when this engine is the DoP (DSD over PCM) writer.
    pub fn is_dop(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            matches!(self, Self::AlsaDop { .. })
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Append audio source.
    /// For ALSA Direct: pushes to the source queue for gapless transition.
    /// For Rodio: delegates to Sink's built-in queue.
    pub fn append<S>(&mut self, source: S) -> Result<(), String>
    where
        S: Source<Item = f32> + Send + 'static,
    {
        match self {
            Self::Rodio { sink } => {
                sink.append(source);
                Ok(())
            }
            Self::Direct {
                label,
                is_playing,
                should_stop,
                position_frames,
                source_queue,
                source_transition,
                ..
            } => {
                let is_first = source_queue.is_empty() && !is_playing.load(Ordering::SeqCst);

                // Box the source iterator and push to queue
                let boxed: BoxedSampleIter = Box::new(source.into_iter());
                source_queue.push(boxed);

                if is_first {
                    // First source: reset position, clear stop, start playing
                    position_frames.store(0, Ordering::SeqCst);
                    should_stop.store(false, Ordering::SeqCst);
                    source_transition.store(false, Ordering::SeqCst);
                    is_playing.store(true, Ordering::SeqCst);
                    log::info!("[{label}] First source queued, playback starting");
                } else {
                    log::info!("[{label}] Source queued for gapless transition");
                }

                Ok(())
            }
            #[cfg(target_os = "linux")]
            Self::Jack {
                is_playing,
                should_stop,
                position_frames,
                source_queue,
                source_transition,
                graph_rate,
                ..
            } => {
                let is_first = source_queue.is_empty() && !is_playing.load(Ordering::SeqCst);
                // Resample the track-native source to the JACK graph rate (stereo) so
                // the feeder/ring always carry graph-rate interleaved stereo f32.
                let resampled = rodio::source::UniformSourceIterator::new(
                    source,
                    std::num::NonZero::new(2u16).unwrap(),
                    std::num::NonZero::new(*graph_rate).unwrap(),
                );
                let boxed: BoxedSampleIter = Box::new(resampled);
                source_queue.push(boxed);
                if is_first {
                    position_frames.store(0, Ordering::SeqCst);
                    should_stop.store(false, Ordering::SeqCst);
                    source_transition.store(false, Ordering::SeqCst);
                    is_playing.store(true, Ordering::SeqCst);
                    log::info!("[JACK Engine] First source queued, playback starting");
                } else {
                    log::info!("[JACK Engine] Source queued for gapless transition");
                }
                Ok(())
            }
            #[cfg(target_os = "linux")]
            Self::AlsaDop { .. } => {
                Err("cannot append a PCM source to a DoP engine".to_string())
            }
        }
    }

    /// Play (unpause)
    pub fn play(&self) {
        match self {
            Self::Rodio { sink } => sink.play(),
            Self::Direct {
                label, is_playing, ..
            } => {
                log::info!("[{label}] Resume requested");
                is_playing.store(true, Ordering::SeqCst);
            }
            #[cfg(target_os = "linux")]
            Self::Jack { is_playing, .. } => {
                log::info!("[JACK Engine] Resume requested");
                is_playing.store(true, Ordering::SeqCst);
            }
            #[cfg(target_os = "linux")]
            Self::AlsaDop { is_playing, .. } => {
                log::info!("[DoP Engine] Resume requested");
                is_playing.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Pause
    pub fn pause(&self) {
        match self {
            Self::Rodio { sink } => sink.pause(),
            Self::Direct {
                label, is_playing, ..
            } => {
                log::info!("[{label}] Pause requested");
                is_playing.store(false, Ordering::SeqCst);
            }
            #[cfg(target_os = "linux")]
            Self::Jack { is_playing, .. } => {
                log::info!("[JACK Engine] Pause requested");
                is_playing.store(false, Ordering::SeqCst);
            }
            #[cfg(target_os = "linux")]
            Self::AlsaDop { is_playing, .. } => {
                // The writer keeps feeding 0x69 DSD silence while paused so
                // the DAC stays locked in DSD mode (no pop on resume).
                log::info!("[DoP Engine] Pause requested (DSD silence keeps flowing)");
                is_playing.store(false, Ordering::SeqCst);
            }
        }
    }

    /// Stop playback and release resources.
    /// For ALSA Direct, signals the writer thread and waits for it to exit.
    /// The Drop impl handles the same cleanup if stop() is not called explicitly.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    /// Internal stop logic shared by stop() and Drop
    fn stop_inner(&mut self) {
        match self {
            Self::Rodio { sink } => {
                sink.stop();
            }
            Self::Direct {
                stream,
                label,
                is_playing,
                should_stop,
                playback_thread,
                ..
            } => {
                if should_stop.load(Ordering::SeqCst) {
                    return; // Already stopped
                }
                log::info!("[{label}] Stop requested");
                should_stop.store(true, Ordering::SeqCst);
                is_playing.store(false, Ordering::SeqCst);

                if let Some(handle) = playback_thread.take() {
                    if handle.join().is_err() {
                        log::error!("[{label}] playback thread panicked before stop completed");
                    }
                }

                if let Err(e) = stream.stop() {
                    log::warn!("[{label}] Stop failed: {}", e);
                }
            }
            #[cfg(target_os = "linux")]
            Self::Jack {
                is_playing,
                should_stop,
                feeder_thread,
                ..
            } => {
                if should_stop.load(Ordering::SeqCst) {
                    return;
                }
                log::info!("[JACK Engine] Stop requested");
                should_stop.store(true, Ordering::SeqCst);
                is_playing.store(false, Ordering::SeqCst);
                if let Some(handle) = feeder_thread.take() {
                    if handle.join().is_err() {
                        log::error!("[JACK Engine] feeder thread panicked before stop completed");
                    }
                }
                // JackStream's Drop deactivates the client + unregisters the ports.
            }
            #[cfg(target_os = "linux")]
            Self::AlsaDop {
                stream,
                is_playing,
                should_stop,
                writer_thread,
                ..
            } => {
                if should_stop.load(Ordering::SeqCst) {
                    return;
                }
                log::info!("[DoP Engine] Stop requested");
                should_stop.store(true, Ordering::SeqCst);
                is_playing.store(false, Ordering::SeqCst);
                if let Some(handle) = writer_thread.take() {
                    if handle.join().is_err() {
                        log::error!("[DoP Engine] writer thread panicked before stop completed");
                    }
                }
                if let Err(e) = stream.stop() {
                    log::warn!("[DoP Engine] Stop failed: {}", e);
                }
            }
        }
    }

    /// Set volume (0.0 - 1.0)
    pub fn set_volume(&self, volume: f32) {
        match self {
            Self::Rodio { sink } => sink.set_volume(volume),
            Self::Direct {
                stream,
                label,
                hardware_volume_control,
                hardware_volume_active,
                hardware_volume_events,
                ..
            } => {
                if hardware_volume_active.load(Ordering::SeqCst) {
                    if let Some(control) = hardware_volume_control {
                        if let Err(error) = stream.set_hardware_volume(control, volume) {
                            log::warn!("[{label}] Hardware volume failed: {error}");
                            hardware_volume_active.store(false, Ordering::SeqCst);
                            hardware_volume_events(HardwareVolumeEvent::Unavailable(error));
                        }
                    }
                } else {
                    log::debug!(
                        "[{label}] Hardware volume control disabled (use DAC/amplifier)"
                    );
                }
            }
            #[cfg(target_os = "linux")]
            Self::Jack { .. } => {
                // JACK output volume is controlled in the JACK graph / DAW; the
                // feeder writes unattenuated f32. (Software volume could later be
                // applied by scaling in the feeder.)
            }
            #[cfg(target_os = "linux")]
            Self::AlsaDop { .. } => {
                // ANY gain applied to DoP words breaks the marker sequence —
                // volume must be controlled at the DAC/amplifier.
                log::debug!("[DoP Engine] Volume is fixed during DoP playback");
            }
        }
    }

    /// Check if playback queue is empty (all sources consumed, not playing)
    pub fn empty(&self) -> bool {
        match self {
            Self::Rodio { sink } => sink.empty(),
            Self::Direct {
                is_playing,
                source_queue,
                ..
            } => !is_playing.load(Ordering::SeqCst) && source_queue.is_empty(),
            #[cfg(target_os = "linux")]
            Self::Jack {
                is_playing,
                source_queue,
                ..
            } => !is_playing.load(Ordering::SeqCst) && source_queue.is_empty(),
            #[cfg(target_os = "linux")]
            Self::AlsaDop {
                is_playing,
                source_queue,
                ..
            } => !is_playing.load(Ordering::SeqCst) && source_queue.is_empty(),
        }
    }

    /// Check if a gapless source transition just happened.
    /// Returns true once, then resets the flag.
    pub fn take_source_transition(&self) -> bool {
        match self {
            Self::Rodio { .. } => false,
            Self::Direct {
                source_transition, ..
            } => source_transition
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            #[cfg(target_os = "linux")]
            Self::Jack {
                source_transition, ..
            } => source_transition
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            #[cfg(target_os = "linux")]
            Self::AlsaDop {
                source_transition, ..
            } => source_transition
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
        }
    }

    /// Get current position in seconds (for ALSA Direct only)
    #[allow(dead_code)]
    pub fn position_secs(&self) -> Option<u64> {
        match self {
            Self::Rodio { .. } => None,
            Self::Direct {
                position_frames,
                stream,
                ..
            } => {
                let frames = position_frames.load(Ordering::SeqCst);
                let sample_rate = stream.sample_rate() as u64;
                Some(frames / sample_rate)
            }
            #[cfg(target_os = "linux")]
            Self::Jack {
                position_frames,
                graph_rate,
                ..
            } => {
                let frames = position_frames.load(Ordering::SeqCst);
                Some(frames / (*graph_rate as u64).max(1))
            }
            #[cfg(target_os = "linux")]
            Self::AlsaDop {
                position_frames,
                stream,
                ..
            } => {
                let frames = position_frames.load(Ordering::SeqCst);
                Some(frames / (stream.sample_rate() as u64).max(1))
            }
        }
    }

    /// Get duration in seconds (for ALSA Direct only)
    #[allow(dead_code)]
    pub fn duration_secs(&self) -> Option<u64> {
        match self {
            Self::Rodio { .. } => None,
            Self::Direct {
                duration_frames,
                stream,
                ..
            } => {
                let frames = duration_frames.load(Ordering::SeqCst);
                let sample_rate = stream.sample_rate() as u64;
                Some(frames / sample_rate)
            }
            #[cfg(target_os = "linux")]
            Self::Jack {
                duration_frames,
                graph_rate,
                ..
            } => {
                let frames = duration_frames.load(Ordering::SeqCst);
                Some(frames / (*graph_rate as u64).max(1))
            }
            #[cfg(target_os = "linux")]
            Self::AlsaDop { .. } => None,
        }
    }

    /// Check if using ALSA Direct engine
    #[allow(dead_code)]
    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct { .. })
    }
}

/// Single long-lived writer thread for any bit-perfect direct sink.
///
/// Continuously reads samples from the current source and writes to the sink.
/// When a source ends, seamlessly picks up the next one from the queue
/// (gapless transition). If no next source is available, drains the ALSA
/// buffer and waits for the next source or a stop signal.
///
/// The source is consumed in ~10 ms pieces instead of the historical fixed
/// 8192-frame blocks. This does not change PCM data or device parameters; it
/// only keeps the passive visualizer tap from receiving ~186 ms bursts at
/// 44.1 kHz. Sinks that can measure their queued frames then point snapshots
/// behind the submitted PCM at the part currently reaching the DAC.
///
/// Generic rather than duplicated: ALSA and WASAPI need byte-identical
/// behaviour here — same pacing, gapless hand-off and drain — and two copies
/// of this state machine would drift.
const DIRECT_PCM_FEED_TARGET_MS: u64 = 10;
const DIRECT_PCM_FEED_MIN_FRAMES: u64 = 64;
const DIRECT_PCM_FEED_MAX_FRAMES: u64 = 8192;

fn direct_pcm_feed_frames(sample_rate: u32) -> usize {
    let frames = u64::from(sample_rate)
        .saturating_mul(DIRECT_PCM_FEED_TARGET_MS)
        .div_ceil(1000)
        .clamp(DIRECT_PCM_FEED_MIN_FRAMES, DIRECT_PCM_FEED_MAX_FRAMES);
    frames as usize
}

fn direct_writer_thread<S: qbz_audio::backend::DirectSink + ?Sized>(
    stream: Arc<S>,
    is_playing: Arc<AtomicBool>,
    should_stop: Arc<AtomicBool>,
    position_frames: Arc<AtomicU64>,
    duration_frames: Arc<AtomicU64>,
    source_queue: Arc<SourceQueue<BoxedSampleIter>>,
    source_transition: Arc<AtomicBool>,
    channels: u16,
    label: &'static str,
    visualizer_tap: Option<VisualizerTap>,
) {
    let chunk_frames = direct_pcm_feed_frames(stream.sample_rate());
    let chunk_samples = chunk_frames * channels as usize;
    let mut buffer_f32 = Vec::with_capacity(chunk_samples);
    let mut current_source: Option<BoxedSampleIter> = None;
    let mut total_frames: u64 = 0;
    let mut queued_frames = 0u64;
    let mut visualizer_was_enabled = false;
    let mut delay_error_reported = false;

    if let Some(tap) = visualizer_tap.as_ref() {
        tap.clear_output_delay();
    }
    log::info!(
        "[{label}] Writer thread started (gapless-capable, viz feed {} frames / ~{} ms)",
        chunk_frames,
        DIRECT_PCM_FEED_TARGET_MS
    );

    'thread: loop {
        // Check global stop
        if should_stop.load(Ordering::SeqCst) {
            log::info!("[{label}] Stop signal, writer thread exiting");
            break 'thread;
        }

        // If no current source, try to get one
        if current_source.is_none() {
            // Wait for a source (with 100ms timeout to recheck stop flag)
            match source_queue.wait_for_source(Duration::from_millis(100)) {
                Some(src) => {
                    current_source = Some(src);
                    total_frames = 0;
                    position_frames.store(0, Ordering::SeqCst);
                    log::info!("[{label}] Acquired new source from queue");
                }
                None => {
                    // No source available, loop back to check stop
                    continue 'thread;
                }
            }
        }

        // Wait while paused
        while !is_playing.load(Ordering::SeqCst) {
            if should_stop.load(Ordering::SeqCst) {
                break 'thread;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Keep the passive tap at the previous audible position while this
        // next piece is pulled rapidly from the source. Once the sink accepts it,
        // the delay query below advances the tap by exactly what the device
        // consumed. Shared/callback backends leave this offset at zero.
        let visualizer_enabled = visualizer_tap
            .as_ref()
            .map(VisualizerTap::is_enabled)
            .unwrap_or(false);
        if visualizer_enabled {
            if !visualizer_was_enabled {
                match stream.playback_delay_frames() {
                    Ok(delay) => queued_frames = delay,
                    Err(error) => {
                        if !delay_error_reported {
                            log::warn!(
                                "[{label}] Visualizer delay query failed; using paced feed without playhead compensation: {}",
                                error
                            );
                            delay_error_reported = true;
                        }
                        queued_frames = 0;
                    }
                }
            }
            if let Some(tap) = visualizer_tap.as_ref() {
                tap.set_output_delay_frames(
                    queued_frames.saturating_add(chunk_frames as u64),
                    channels,
                );
            }
        } else if visualizer_was_enabled {
            if let Some(tap) = visualizer_tap.as_ref() {
                tap.clear_output_delay();
            }
        }
        visualizer_was_enabled = visualizer_enabled;

        // Fill buffer from current source.
        buffer_f32.clear();
        let source = current_source.as_mut().unwrap();
        let mut source_ended = false;

        for _ in 0..chunk_samples {
            match source.next() {
                Some(sample) => buffer_f32.push(sample),
                None => {
                    source_ended = true;
                    break;
                }
            }
        }

        let frames_in_chunk = buffer_f32.len() / channels as usize;
        if visualizer_enabled {
            if let Some(tap) = visualizer_tap.as_ref() {
                tap.set_output_delay_frames(
                    queued_frames.saturating_add(frames_in_chunk as u64),
                    channels,
                );
            }
        }

        // Write whatever we have to the direct sink (even partial chunks on source end)
        if !buffer_f32.is_empty() {
            if let Err(e) = stream.write_f32(&buffer_f32) {
                log::error!("[{label}] Write failed: {}", e);
                break 'thread;
            }

            total_frames += frames_in_chunk as u64;
            position_frames.store(total_frames, Ordering::SeqCst);
            duration_frames.store(total_frames, Ordering::SeqCst);

            if visualizer_enabled {
                match stream.playback_delay_frames() {
                    Ok(delay) => {
                        queued_frames = delay;
                        if let Some(tap) = visualizer_tap.as_ref() {
                            tap.set_output_delay_frames(queued_frames, channels);
                        }
                    }
                    Err(error) => {
                        if !delay_error_reported {
                            log::warn!(
                                "[{label}] Visualizer delay query failed; using paced feed without playhead compensation: {}",
                                error
                            );
                            delay_error_reported = true;
                        }
                        queued_frames = 0;
                        if let Some(tap) = visualizer_tap.as_ref() {
                            tap.clear_output_delay();
                        }
                    }
                }
            }
        }

        if source_ended {
            log::info!(
                "[{label}] Source ended (total frames: {})",
                total_frames
            );

            // Try to get next source immediately (gapless transition)
            match source_queue.try_pop() {
                Some(next_src) => {
                    log::info!("[{label}] Gapless transition to next source");
                    current_source = Some(next_src);
                    total_frames = 0;
                    position_frames.store(0, Ordering::SeqCst);
                    // Signal that a transition happened
                    source_transition.store(true, Ordering::SeqCst);
                    // Continue immediately — no drain, no gap
                }
                None => {
                    // No next source — this is a natural end of playback
                    log::info!("[{label}] No next source, draining the device buffer");
                    if let Err(e) = stream.drain() {
                        log::warn!("[{label}] Drain failed: {}", e);
                    }
                    current_source = None;
                    is_playing.store(false, Ordering::SeqCst);
                    // Don't break — stay alive waiting for next append()
                }
            }
        }
    }

    is_playing.store(false, Ordering::SeqCst);
    if let Some(tap) = visualizer_tap.as_ref() {
        tap.clear_output_delay();
    }
    log::info!("[{label}] Writer thread finished");
}

/// Single long-lived feeder thread for JACK (#263 Tier 3).
///
/// Mirrors `direct_writer_thread`, but writes graph-rate interleaved STEREO f32
/// into the JACK client's lock-free ring buffer via `JackStream::write_f32`
/// (the RT process callback drains it), pacing itself when the ring is full.
/// Sources are resampled to the graph rate + stereo at `append` time.
#[cfg(target_os = "linux")]
fn jack_feeder_thread(
    stream: Arc<JackStream>,
    is_playing: Arc<AtomicBool>,
    should_stop: Arc<AtomicBool>,
    position_frames: Arc<AtomicU64>,
    duration_frames: Arc<AtomicU64>,
    source_queue: Arc<SourceQueue<BoxedSampleIter>>,
    source_transition: Arc<AtomicBool>,
) {
    const CHUNK_FRAMES: usize = 4096;
    const CHANNELS: usize = 2;
    let chunk_samples = CHUNK_FRAMES * CHANNELS;
    let mut buffer_f32: Vec<f32> = Vec::with_capacity(chunk_samples);
    let mut current_source: Option<BoxedSampleIter> = None;
    let mut total_frames: u64 = 0;

    log::info!("[JACK Engine] Feeder thread started");

    'thread: loop {
        if should_stop.load(Ordering::SeqCst) {
            break 'thread;
        }
        if current_source.is_none() {
            match source_queue.wait_for_source(Duration::from_millis(100)) {
                Some(src) => {
                    current_source = Some(src);
                    total_frames = 0;
                    position_frames.store(0, Ordering::SeqCst);
                }
                None => continue 'thread,
            }
        }
        while !is_playing.load(Ordering::SeqCst) {
            if should_stop.load(Ordering::SeqCst) {
                break 'thread;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        buffer_f32.clear();
        let source = current_source.as_mut().unwrap();
        let mut source_ended = false;
        for _ in 0..chunk_samples {
            match source.next() {
                Some(s) => buffer_f32.push(s),
                None => {
                    source_ended = true;
                    break;
                }
            }
        }

        // Write to the ring, paced: write_f32 returns frames accepted; a full
        // ring returns fewer (or 0) and we wait for the RT callback to drain.
        let mut off_samples = 0usize;
        while off_samples < buffer_f32.len() {
            if should_stop.load(Ordering::SeqCst) {
                break 'thread;
            }
            let frames = stream.write_f32(&buffer_f32[off_samples..]);
            if frames == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            off_samples += frames * CHANNELS;
            total_frames += frames as u64;
            position_frames.store(total_frames, Ordering::SeqCst);
            duration_frames.store(total_frames, Ordering::SeqCst);
        }

        if source_ended {
            match source_queue.try_pop() {
                Some(next_src) => {
                    current_source = Some(next_src);
                    total_frames = 0;
                    position_frames.store(0, Ordering::SeqCst);
                    source_transition.store(true, Ordering::SeqCst);
                }
                None => {
                    current_source = None;
                    is_playing.store(false, Ordering::SeqCst);
                }
            }
        }
    }

    is_playing.store(false, Ordering::SeqCst);
    log::info!("[JACK Engine] Feeder thread finished");
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Single long-lived writer thread for DoP (DSD over PCM).
///
/// Mirrors `direct_writer_thread`'s shape: pulls pre-packed S32 DoP words from
/// the current source, writes them VERBATIM, and picks up the next queued
/// source seamlessly (gapless DSD). Differences forced by the format:
/// - pause writes 0x69 DSD silence (with valid alternating markers) instead
///   of going quiet, so the DAC stays locked in DSD mode;
/// - stop / end-of-queue pads ~150 ms of DSD silence before the stream
///   closes (DACs pop when a DSD stream stops mid-pattern).
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn dop_writer_thread(
    stream: Arc<AlsaDirectStream>,
    is_playing: Arc<AtomicBool>,
    should_stop: Arc<AtomicBool>,
    position_frames: Arc<AtomicU64>,
    source_queue: Arc<SourceQueue<BoxedDopIter>>,
    replacement: Arc<Mutex<Option<DopReplacement>>>,
    source_transition: Arc<AtomicBool>,
    channels: u16,
    native: bool,
) {
    const CHUNK_FRAMES: usize = 4096;
    let chunk_words = CHUNK_FRAMES * channels as usize;
    let carrier = stream.sample_rate() as usize;
    let mut silence_packer = qbz_dsd::DopPacker::new();
    let mut silence_buf: Vec<i32> = Vec::new();
    let mut buf: Vec<i32> = Vec::with_capacity(chunk_words);
    let mut current: Option<BoxedDopIter> = None;
    let mut had_source = false;

    let write_silence = |packer: &mut qbz_dsd::DopPacker,
                             silence_buf: &mut Vec<i32>,
                             frames: usize| {
        silence_buf.clear();
        if native {
            // Native DSD silence: 0x69 in every byte lane, no DoP markers.
            silence_buf.resize(frames * channels as usize, qbz_dsd::NATIVE_DSD_SILENCE_U32);
        } else {
            packer.silence(frames, channels, silence_buf);
        }
        if let Err(e) = stream.write_dop_i32(silence_buf) {
            log::warn!("[DoP Engine] Silence write failed: {}", e);
        }
    };

    log::info!("[DoP Engine] Writer thread started (gapless-capable)");
    'thread: loop {
        if should_stop.load(Ordering::SeqCst) {
            write_silence(&mut silence_packer, &mut silence_buf, carrier * 150 / 1000);
            log::info!("[DoP Engine] Stop signal, writer thread exiting");
            break 'thread;
        }

        if let Some((source, position_base)) = replacement.lock().unwrap().take() {
            // Do not drop/reopen ALSA here: that makes the DAC leave DSD mode
            // and is the source of the loud click users hear on a naive seek.
            // A short valid-silence bridge gives both DoP and native receivers
            // a clean boundary while preserving exclusive ownership.
            write_silence(&mut silence_packer, &mut silence_buf, carrier * 150 / 1000);
            current = Some(source);
            had_source = true;
            position_frames.store(position_base, Ordering::SeqCst);
            source_transition.store(false, Ordering::SeqCst);
            log::info!("[DoP Engine] Source replaced in-place for seek");
        }

        if current.is_none() {
            match source_queue.wait_for_source(Duration::from_millis(100)) {
                Some(src) => {
                    current = Some(src);
                    if had_source {
                        source_transition.store(true, Ordering::SeqCst);
                    }
                    had_source = true;
                    position_frames.store(0, Ordering::SeqCst);
                    log::info!("[DoP Engine] Acquired new DoP source");
                }
                None => continue 'thread,
            }
        }

        if !is_playing.load(Ordering::SeqCst) {
            // Paused: keep the DAC locked in DSD with real DSD silence. The
            // blocking PCM write self-paces this loop (~100 ms per chunk).
            write_silence(&mut silence_packer, &mut silence_buf, carrier / 10);
            continue 'thread;
        }

        buf.clear();
        let source = current.as_mut().unwrap();
        let mut source_ended = false;
        for _ in 0..chunk_words {
            match source.next() {
                Some(w) => buf.push(w),
                None => {
                    source_ended = true;
                    break;
                }
            }
        }

        if !buf.is_empty() {
            if let Err(e) = stream.write_dop_i32(&buf) {
                // Match PCM ALSA: a hard write failure must stop the writer.
                // Continuing would desync DoP markers (harsh noise) and leave
                // exclusive mode stuck while position still advances.
                log::error!("[DoP Engine] Write failed: {e} — stopping writer");
                is_playing.store(false, Ordering::SeqCst);
                should_stop.store(true, Ordering::SeqCst);
                break 'thread;
            }
            position_frames.fetch_add((buf.len() / channels as usize) as u64, Ordering::SeqCst);
        }

        if source_ended {
            current = None;
            if source_queue.is_empty() {
                write_silence(&mut silence_packer, &mut silence_buf, carrier * 150 / 1000);
                is_playing.store(false, Ordering::SeqCst);
                log::info!("[DoP Engine] Source ended, no next source (padded DSD silence)");
            }
            // else: the queued next source is picked up on the next iteration
            // with the PCM still running — the gapless DSD transition.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::direct_pcm_feed_frames;

    #[test]
    fn direct_pcm_feed_stays_near_ten_ms_across_pcm_rates() {
        assert_eq!(direct_pcm_feed_frames(44_100), 441);
        assert_eq!(direct_pcm_feed_frames(48_000), 480);
        assert_eq!(direct_pcm_feed_frames(96_000), 960);
        assert_eq!(direct_pcm_feed_frames(192_000), 1_920);
        assert_eq!(direct_pcm_feed_frames(768_000), 7_680);
    }

    #[test]
    fn direct_pcm_feed_has_defensive_bounds() {
        assert_eq!(direct_pcm_feed_frames(0), 64);
        assert_eq!(direct_pcm_feed_frames(u32::MAX), 8_192);
    }
}
