//! Now-playing model — the single data source behind the bar's `np*` bridge
//! properties (Slint `NowPlayingState`).
//!
//! Holds the track meta + transport, the QUALITY STAMP state (catalog tier /
//! detail, the delivered-vs-catalog downgrade block) and the cast/remote
//! flags. Every field is published through ONE `publish()` — a second
//! near-copy of it was the reason the cache overlay and the quality fields
//! could disagree, so there is exactly one now.
//!
//! The arithmetic behind the downgrade block lives in `quality_state.rs`
//! (pure, unit-tested); the two output LEDs are derived from AudioSettings in
//! `output_labels.rs`. They follow the SETTINGS, not the track — but they are
//! DECIDED when a stream opens, so besides `settings_qt::publish_snapshot`
//! they are re-derived on shell entry (`publish_current`), on the track edge
//! (`playback_qt::refresh_now_playing`) and on the stream edge
//! (`set_effective_stream`, only when the delivered params actually move).
//! Without those edges the stamp only refreshed when the user changed page.
//!
//! POC-NOTE: playback transport is wired (playback_qt poll pump). Cast is NOT
//! in the POC build (no qbz-cast dep), so `set_cast_session` exists as the
//! wiring seam; `set_remote` / `set_remote_volume_locked` ARE live — written
//! by the Qobuz Connect port (qconnect_event_sink_qt's badge refresh + the
//! facade's disconnect tail).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use cxx_qt_lib::{QList, QString};

use crate::quality_state::Delivered;

#[derive(Clone, Default)]
pub struct NowPlayingModel {
    pub has_track: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: String,
    pub artist_id: String,
    /// "Playing from" ORIGIN of the current track — the container the queue was
    /// launched from, re-derived per track change (never a stale global).
    /// `context_kind` is "album" | "artist" | "playlist" | "label";
    /// `context_id` is that container's navigation id. Feeds the song-card
    /// layers glyph (SongCard.slint `ctx-kind` / `ctx-id`, :151-156). Empty id
    /// = no origin -> the card falls back to the track's own album.
    pub context_kind: String,
    pub context_id: String,
    pub artwork_path: String,
    pub elapsed_secs: i32,
    pub duration_secs: i32,
    pub playing: bool,
    pub loading: bool,
    /// Streaming buffer fill 0..1 (seekbar cache overlay); 0 = not streaming.
    pub cache: f32,
    /// SEEK LOCK 0..1 — the furthest fraction of the track the user may seek
    /// to (PARITY-DEBT #15, Slint `NowPlayingState.seekable-max`). While a
    /// stream is downloading this is `buffer_progress` clamped to 0..1; a
    /// fully-available track is 1.0. Distinct from `cache`, which is the
    /// decorative overlay of the same fill: this one is enforced.
    pub seekable_max: f32,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    /// 0 off / 1 all / 2 one.
    pub repeat_mode: i32,
    /// CATALOG tier: "hires" | "mp3" | "lossless" | "cd" | "".
    pub quality_tier: String,
    /// CATALOG detail, e.g. "24-bit / 96 kHz".
    pub quality_detail: String,
    /// The OPENED STREAM's bit-perfect mode, straight from the engine:
    /// "direct" | "plugin" | "disabled" | "unknown". Never derived from
    /// settings - that is what the mode LED already does.
    pub bit_perfect_mode: String,
    /// Delivered-vs-catalog block (downgrade arrow + tooltip cause).
    pub delivered: Delivered,
    /// Last delivered stream params seen from the engine, so a poll tick that
    /// changes nothing costs no Qt-thread hop.
    pub eff_rate_hz: u32,
    pub eff_bits: u32,
    /// A peer Qobuz Connect renderer owns playback.
    pub is_remote: bool,
    /// The active PEER renderer disallows remote volume control (the remote
    /// half of Slint `NowPlayingState.volume-locked`, contract §11.3). Kept
    /// OUT of the settings-derived `np_volume_locked` — that one is
    /// republished on every settings/track edge and would clobber this.
    pub remote_volume_locked: bool,
    pub remote_volume_pending: bool,
    /// Active renderer / cast target name; empty when local.
    pub cast_target: String,
    /// A Chromecast/DLNA session is connected.
    pub cast_active: bool,
    /// "cast" | "dlna".
    pub cast_protocol: String,
}

/// The idle bar: no track, full volume, no badge, seek unlocked (the Slint
/// `NowPlayingState` defaults — `seekable-max: 1.0`, state.slint:4402).
fn idle() -> NowPlayingModel {
    NowPlayingModel {
        volume: 1.0,
        seekable_max: 1.0,
        ..NowPlayingModel::default()
    }
}

static MODEL: Mutex<Option<NowPlayingModel>> = Mutex::new(None);
static WAVEFORM_REVISION: AtomicU64 = AtomicU64::new(0);
static WAVEFORM_TRACK: AtomicU64 = AtomicU64::new(0);

fn with_model<T>(f: impl FnOnce(&mut NowPlayingModel) -> T) -> (T, NowPlayingModel) {
    let mut guard = MODEL.lock().unwrap();
    let m = guard.get_or_insert_with(idle);
    let out = f(m);
    (out, m.clone())
}

fn mutate(f: impl FnOnce(&mut NowPlayingModel)) {
    let (_, snapshot) = with_model(f);
    publish(&snapshot);
}

/// Push the full model onto the bridge (Qt thread hop). The ONLY publisher —
/// every setter funnels through here so no field can go stale behind another.
fn publish(m: &NowPlayingModel) {
    // Mirror the transport onto the visualizer tap: a paused player parks the
    // FFT producer instead of re-analyzing a stale ring buffer (the Slint
    // playback.rs does the same). Cheap atomics, safe off the Qt thread.
    crate::viz_qt::set_paused(!m.playing);
    let m = m.clone();
    crate::player_bridge::ui(move |mut b| {
        let progress = if m.duration_secs > 0 {
            (m.elapsed_secs as f32 / m.duration_secs as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        b.as_mut().set_np_has_track(m.has_track);
        b.as_mut().set_np_title(QString::from(m.title.as_str()));
        b.as_mut().set_np_artist(QString::from(m.artist.as_str()));
        b.as_mut().set_np_album(QString::from(m.album.as_str()));
        b.as_mut()
            .set_np_album_id(QString::from(m.album_id.as_str()));
        b.as_mut()
            .set_np_artist_id(QString::from(m.artist_id.as_str()));
        b.as_mut()
            .set_np_context_kind(QString::from(m.context_kind.as_str()));
        b.as_mut()
            .set_np_context_id(QString::from(m.context_id.as_str()));
        b.as_mut()
            .set_np_artwork_path(QString::from(m.artwork_path.as_str()));
        b.as_mut().set_np_elapsed_secs(m.elapsed_secs);
        b.as_mut().set_np_duration_secs(m.duration_secs);
        b.as_mut().set_np_progress(progress);
        b.as_mut().set_np_cache_progress(m.cache);
        b.as_mut().set_np_seekable_max(m.seekable_max);
        b.as_mut().set_np_playing(m.playing);
        b.as_mut().set_np_loading(m.loading);
        b.as_mut().set_np_volume(m.volume);
        b.as_mut().set_np_muted(m.muted);
        b.as_mut().set_np_shuffle(m.shuffle);
        b.as_mut().set_np_repeat_mode(m.repeat_mode);
        // --- Quality stamp ------------------------------------------------
        b.as_mut()
            .set_np_quality_tier(QString::from(m.quality_tier.as_str()));
        let detail = QString::from(m.quality_detail.as_str());
        b.as_mut().set_np_quality_detail(detail.clone());
        // Legacy alias of the same value (pre-contract Qt name).
        b.as_mut().set_np_quality_label(detail);
        b.as_mut()
            .set_np_bit_perfect_mode(QString::from(m.bit_perfect_mode.as_str()));
        b.as_mut().set_np_quality_downgraded(m.delivered.downgraded);
        b.as_mut()
            .set_np_quality_true_detail(QString::from(m.delivered.true_detail.as_str()));
        b.as_mut()
            .set_np_quality_effective_tier(QString::from(m.delivered.effective_tier.as_str()));
        b.as_mut()
            .set_np_quality_limit_cause(m.delivered.limit_cause);
        // A3: the delivered stream params as scalars — the Spectral Ribbon
        // overlay header reads them ("Audio Stream, {rate} Hz, {bits} bits",
        // ImmersiveSpectralOverlay.slint:45). 0 = not reported yet.
        b.as_mut().set_np_eff_rate_hz(m.eff_rate_hz as i32);
        b.as_mut().set_np_eff_bits(m.eff_bits as i32);
        // --- Cast / remote ------------------------------------------------
        b.as_mut().set_np_is_remote(m.is_remote);
        b.as_mut()
            .set_np_remote_volume_locked(m.remote_volume_locked);
        b.as_mut()
            .set_np_remote_volume_pending(m.remote_volume_pending);
        b.as_mut()
            .set_np_cast_target(QString::from(m.cast_target.as_str()));
        b.as_mut().set_np_cast_active(m.cast_active);
        b.as_mut()
            .set_np_cast_protocol(QString::from(m.cast_protocol.as_str()));
    });
}

/// Seed the bridge properties from the model at shell entry (the model is
/// static, so a logout/login round-trip keeps the last UI toggles — same
/// as the Slint app, where NowPlayingState survives).
pub fn publish_current() {
    let (_, snapshot) = with_model(|_| ());
    publish(&snapshot);
    // The layers-glyph preference (see `publish_show_context_icon`).
    publish_show_context_icon();
    publish_show_skip_ten();
    // Seed the two output LEDs + the volume-lock flag at shell entry too, so
    // the stamp is correct before the first track ever plays (the bridge
    // defaults are the unlit SYST/DEFAULT pair).
    crate::output_labels::publish_current();
}

/// Publish the 512-bin seek waveform only when its analyzer revision moves.
/// Position ticks continue through `publish()` without cloning this document,
/// so an already-rendered waveform is static scenegraph data.
pub fn publish_seek_waveform(track_id: u64) {
    let snapshot = qbz_audio::seek_waveform_snapshot();
    if track_id == 0 || snapshot.track_id != track_id {
        if WAVEFORM_TRACK.swap(track_id, Ordering::Relaxed) != track_id {
            WAVEFORM_REVISION.store(0, Ordering::Relaxed);
            crate::player_bridge::ui(|mut player| {
                player.as_mut().set_np_seek_waveform(QList::default());
                player.as_mut().set_np_seek_waveform_analyzed(0.0);
                player.as_mut().set_np_seek_waveform_complete(false);
            });
        }
        return;
    }
    if WAVEFORM_TRACK.load(Ordering::Relaxed) == track_id
        && WAVEFORM_REVISION.load(Ordering::Relaxed) == snapshot.revision
    {
        return;
    }
    WAVEFORM_TRACK.store(track_id, Ordering::Relaxed);
    WAVEFORM_REVISION.store(snapshot.revision, Ordering::Relaxed);
    let mut bins = QList::<f32>::default();
    for value in snapshot.bins {
        bins.append(value as f32 / 255.0);
    }
    let analyzed = snapshot.analyzed_bins as f32 / qbz_audio::SEEK_WAVEFORM_BINS as f32;
    crate::player_bridge::ui(move |mut player| {
        player.as_mut().set_np_seek_waveform(bins);
        player
            .as_mut()
            .set_np_seek_waveform_analyzed(analyzed.clamp(0.0, 1.0));
        player
            .as_mut()
            .set_np_seek_waveform_complete(snapshot.complete);
    });
}

/// Re-publish the "Show track playing context" preference onto the bar (the
/// song-card layers glyph gate).
///
/// The bridge seeds `show_context_icon` exactly ONCE, in
/// `QbzPlayerRust::default()` (player_bridge.rs:260) — i.e. when the QML engine
/// constructs the singleton, which is before a session exists — and
/// `settings_qt::show_context_icon` (settings_qt.rs:422-424) returns FALSE
/// whenever the playback-preferences store is not open at that instant
/// (`unwrap_or(false)`). Nothing republished it afterwards, so the glyph could
/// latch OFF for the whole run and the Settings > Playback toggle wrote the DB
/// with no visible effect — a control that renders and does nothing. Call this
/// on shell entry and from the toggle.
pub fn publish_show_context_icon() {
    let show = crate::settings_qt::show_context_icon();
    crate::player_bridge::ui(move |mut b| b.as_mut().set_show_context_icon(show));
}

/// The ±10 s buttons preference (ui_prefs `show_skip_ten`), re-published on
/// shell entry and from the Settings toggle.
pub fn publish_show_skip_ten() {
    let show = crate::settings_qt::pref_bool("show_skip_ten", false);
    crate::player_bridge::ui(move |mut b| b.as_mut().set_show_skip_ten(show));
}

// --- Pure-UI toggles (mutate + republish) --------------------------------

pub fn set_volume(volume: f32) {
    mutate(|m| {
        m.volume = volume.clamp(0.0, 1.0);
        if m.volume > 0.0 {
            m.muted = false;
        }
    });
}

pub fn set_muted(muted: bool) {
    mutate(|m| m.muted = muted);
}

pub fn set_shuffle(shuffle: bool) {
    mutate(|m| m.shuffle = shuffle);
}

pub fn set_repeat_mode(mode: i32) {
    mutate(|m| m.repeat_mode = mode);
}

pub fn repeat_mode() -> i32 {
    with_model(|m| m.repeat_mode).0
}

/// (elapsed_secs, duration_secs) of the current track — the read the hotkeys
/// seek math (2026-08-03 hotkeys-port contract §1.3) makes off the Slint
/// `NowPlayingState` (`keybindings.rs:572-581`: `position_secs` /
/// `duration_secs`, then `clamp(pos ± d, 0, dur) / dur`). The 1 Hz poll-pump
/// base is the Slint base (immersive contract D14).
pub(crate) fn position() -> (i32, i32) {
    with_model(|m| (m.elapsed_secs, m.duration_secs)).0
}

/// The published seek lock (`np_seekable_max`, 0.0 = not yet published).
pub(crate) fn seekable_max() -> f32 {
    with_model(|m| m.seekable_max).0
}

/// (artist_id, title) of the current track — the read the immersive
/// Suggestions loader makes off the Slint `NowPlayingState`
/// (`main.rs:16699-16702`: the panel only has the track id, the seed artist
/// and name come from here). Empty strings when idle.
pub(crate) fn seed_meta() -> (String, String) {
    with_model(|m| (m.artist_id.clone(), m.title.clone())).0
}

/// The transport flag as PUBLISHED — `QbzPlayer.npPlaying`'s own source, and
/// therefore the exact Qt twin of the Slint `NowPlayingState.playing` the
/// miniplayer's row-0 arm reads (`crates/qbz/src/miniplayer.rs:381-385`:
/// *"Index 0 = the current track -> resume if paused (no restart)"*).
///
/// Read here rather than off `player().state.is_playing()` on purpose: this
/// model is fed by the cast publisher (`cast_qt.rs:1173`) and the QConnect
/// mirror as well as by the local poll (`set_position` above), so it stays
/// true while the deck is a cast device and the local engine is idle. The
/// engine-level read would report "paused" there and turn a row-0 click into
/// a PAUSE of the cast session.
pub(crate) fn playing() -> bool {
    with_model(|m| m.playing).0
}

pub fn set_playing(playing: bool) {
    mutate(|m| m.playing = playing);
}

// --- Track meta + position (fed by the poll pump) ------------------------

/// Current-track metadata as published by the playback controller.
pub struct TrackMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: String,
    pub artist_id: String,
    /// The queue track's id + source ("qobuz" | "local" | "plex" | …), stashed
    /// out-of-band for the size-aware large-art feed (D2): local tracks need
    /// the rowid to re-open embedded/folder art, Plex rows route to the 1024
    /// transcode tier, Qobuz rows re-resolve through the registered ImageSet.
    pub track_id: u64,
    pub source: String,
    /// Container origin of THIS track ("album" | "artist" | "playlist" |
    /// "label" + its id). Never optional at this seam: `refresh_now_playing`
    /// resolves the fallback (the track's own album) before it gets here, so a
    /// caller cannot publish a track with no origin at all.
    pub context_kind: String,
    pub context_id: String,
    pub duration_secs: i32,
    pub quality_tier: String,
    pub quality_label: String,
    pub artwork_url: String,
    pub shuffle: bool,
    pub repeat_mode: i32,
}

/// A new current track: full meta swap (art path attaches separately via
/// the artwork pipeline — see `artwork_qt::attach_now_playing`).
///
/// Resets the DELIVERED block: the engine re-derives it once the new stream
/// opens, and the previous track's downgrade state must never linger.
pub fn set_track(meta: TrackMeta) {
    // Stash the artwork seed out-of-band BEFORE the meta swap (the closure
    // moves the fields it publishes): the small feed resolves through
    // `artwork_qt::attach_now_playing`, the size-aware large feed (D2) reads
    // ART_SEED.
    *ARTWORK_URL.lock().unwrap() = meta.artwork_url.clone();
    *ART_SEED.lock().unwrap() = ArtSeed {
        url: meta.artwork_url.clone(),
        track_id: meta.track_id,
        album_id: meta.album_id.clone(),
        source: meta.source.clone(),
    };
    mutate(|m| {
        m.has_track = true;
        m.title = meta.title;
        m.artist = meta.artist;
        m.album = meta.album;
        m.album_id = meta.album_id;
        m.artist_id = meta.artist_id;
        m.context_kind = meta.context_kind;
        m.context_id = meta.context_id;
        m.duration_secs = meta.duration_secs;
        m.elapsed_secs = 0;
        m.cache = 0.0;
        m.quality_tier = meta.quality_tier;
        m.quality_detail = meta.quality_label;
        m.bit_perfect_mode = String::new();
        m.delivered = Delivered::default();
        m.eff_rate_hz = 0;
        m.eff_bits = 0;
        m.artwork_path = String::new();
        m.loading = true;
        m.playing = true;
        m.shuffle = meta.shuffle;
        m.repeat_mode = meta.repeat_mode;
    });
}

/// Seed the CATALOG maximum + the requested tier for the new current track
/// (playback.rs `refresh_now_playing_meta`: the TRACK_MAX / REQUESTED stores).
/// Resolved ONCE per track change — never per poll tick, because the request
/// tier is a preferences read.
///
/// `governed` = the streaming-quality preference shapes this track's request
/// (Qobuz-sourced, not local/Plex); ungoverned tracks keep the cause line off.
/// Publishes nothing on its own: the seed only changes what the NEXT
/// `set_effective_stream` computes.
pub fn set_catalog_quality(bit_depth: Option<u32>, sample_rate: Option<f64>, governed: bool) {
    crate::quality_state::seed_track(bit_depth, sample_rate, governed);
}

/// The engine's DELIVERED stream params, from the poll tick's PlaybackEvent
/// (`sample_rate` / `bit_depth`; 0 = not reported yet). Re-evaluates the
/// downgrade block and republishes ONLY when the params actually moved, so a
/// steady stream costs one atomic compare per tick and no Qt hop.
pub fn set_effective_stream(eff_rate_hz: u32, eff_bits: u32, bit_perfect: &str) {
    let (changed, snapshot) = with_model(|m| {
        if m.eff_rate_hz == eff_rate_hz
            && m.eff_bits == eff_bits
            && m.bit_perfect_mode == bit_perfect
        {
            return false;
        }
        m.eff_rate_hz = eff_rate_hz;
        m.eff_bits = eff_bits;
        m.bit_perfect_mode = bit_perfect.to_string();
        m.delivered = crate::quality_state::evaluate(eff_rate_hz, eff_bits);
        true
    });
    if changed {
        publish(&snapshot);
        // STREAM edge: the engine has just reported real params, i.e. the
        // output stream is open and the backend/mode are now facts. Re-derive
        // the two LEDs so they are right even if the audio settings moved
        // between the track edge and the stream actually opening. Gated by
        // `changed`, so a steady stream costs nothing — this is not a poll.
        crate::output_labels::publish_current();
    }
}

/// No current track (queue cleared / finished): back to the idle bar. Keeps
/// the user's volume/mute and the cast/remote session (a session outlives the
/// track).
pub fn clear_track() {
    mutate(|m| {
        let kept = NowPlayingModel {
            volume: m.volume,
            muted: m.muted,
            // The seek lock belongs to the STREAM that just ended — an idle
            // bar seeks nothing, but leaving the last stream's fraction here
            // would carry a 0.3 lock into the next track's first tick.
            seekable_max: 1.0,
            is_remote: m.is_remote,
            remote_volume_locked: m.remote_volume_locked,
            remote_volume_pending: m.remote_volume_pending,
            cast_target: m.cast_target.clone(),
            cast_active: m.cast_active,
            cast_protocol: m.cast_protocol.clone(),
            ..NowPlayingModel::default()
        };
        *m = kept;
    });
    crate::quality_state::seed_track(None, None, false);
    // No current track: the large-art feed has nothing to re-resolve (D2).
    *ART_SEED.lock().unwrap() = ArtSeed::default();
    set_artwork_path_large(String::new());
}

// --- Cast / remote (wiring seam — no cast service in the POC build) -------

/// A Chromecast/DLNA session connected or dropped (`cast_service.rs`
/// `push_connection_state`). `protocol` is "cast" | "dlna".
pub fn set_cast_session(active: bool, protocol: &str, target: &str) {
    mutate(|m| {
        m.cast_active = active;
        m.cast_protocol = if active {
            protocol.to_string()
        } else {
            String::new()
        };
        if active {
            m.cast_target = target.to_string();
        } else if !m.is_remote {
            m.cast_target.clear();
        }
    });
}

/// A peer Qobuz Connect renderer took over (or handed back) the transport.
/// THE writer is the qconnect sink's `refresh_now_playing_remote_state`
/// (`qconnect_event_sink_qt.rs`, gated on `transport_connected` — the
/// stale-badge fix); the facade's disconnect tail is the second write site.
/// `target` is the renderer's friendly name, empty when local.
pub fn set_remote(is_remote: bool, target: &str) {
    if !is_remote {
        // Contract §11.1: remote mode ended — drop the lyrics remote anchor.
        // This setter is the ONE choke point every remote-end path funnels
        // through (the qconnect sink's badge refresh and the facade's
        // disconnect tail — which the cast suspend also rides), so the clear
        // lives here instead of the Slint's per-tick poll-loop site
        // (playback.rs:5279): the Qt lyrics getters gate on THIS model flag
        // rather than a lyrics-side ACTIVE atomic, and the poll loop's local
        // fallthrough is skipped while the cast branch runs — exactly the
        // window where a stale anchor would otherwise linger.
        crate::lyrics_qt::clear_remote_anchor();
    }
    mutate(|m| {
        m.is_remote = is_remote;
        if is_remote {
            m.cast_target = target.to_string();
        } else if !m.cast_active {
            m.cast_target.clear();
        }
    });
}

/// Read-only: a peer Qobuz Connect renderer owns playback. The lyrics sync
/// engine gates its remote-anchor read on this (contract §11.1).
pub fn is_remote() -> bool {
    with_model(|m| m.is_remote).0
}

/// Whether playback currently lives on another device (QConnect renderer or
/// a Chromecast/DLNA session). The local engine reports `is_playing=false`
/// in both cases, so an "idle" decision must ask this too.
pub(crate) fn remote_or_cast_active() -> bool {
    with_model(|m| m.is_remote || m.cast_active).0
}

/// Read-only: the current output volume 0..1. The keyboard volume steps read
/// it to compute the next value (the bars read the published property).
pub(crate) fn volume() -> f32 {
    with_model(|m| m.volume).0
}

/// Read-only mirror of `set_remote_volume_locked` — the active peer renderer
/// disallows remote volume control. Half of the hotkeys' `volume_locked`.
pub(crate) fn remote_volume_locked() -> bool {
    with_model(|m| m.remote_volume_locked).0
}

/// The REMOTE volume lock (contract §11.3): the active peer Qobuz Connect
/// renderer disallows remote volume control. Written from the qconnect sink's
/// badge refresh alongside `set_remote`, and cleared by the facade's
/// disconnect tail — publish onto `QbzPlayer.np_remote_volume_locked` (never
/// folded into the settings-derived `np_volume_locked`).
pub fn set_remote_volume_locked(locked: bool) {
    mutate(|m| {
        m.remote_volume_locked = locked;
        if !locked {
            m.remote_volume_pending = false;
        }
    });
}

/// Publish ownership, level and lock together so controls never briefly use
/// the previous device's level during a handoff.
pub fn set_remote_volume_state(
    target: &str,
    volume: f32,
    muted: bool,
    locked: bool,
    pending: bool,
) {
    mutate(|m| {
        m.is_remote = true;
        m.cast_target = target.to_string();
        m.volume = volume.clamp(0.0, 1.0);
        m.muted = muted;
        m.remote_volume_locked = locked;
        m.remote_volume_pending = pending;
    });
}

/// The DELIVERED quality measured by a cast session, which the local poll
/// cannot see (the local player is stopped while casting —
/// `cast_service.rs` `publish_delivered_quality`). `limit_cause` is a
/// `qbz_models::QualityLimit` discriminant already classified by the caller.
pub fn set_cast_delivered(delivered: Delivered) {
    mutate(|m| m.delivered = delivered);
}

/// The artwork url of the current track, stashed for the artwork pipeline.
static ARTWORK_URL: Mutex<String> = Mutex::new(String::new());

/// What the size-aware large-art feed (D2, contract
/// `2026-08-15-immersive-completion` 04 §4) needs to re-resolve the current
/// track at a bigger tier: the raw url, the queue id (local tracks re-open
/// their audio file by rowid), the album id (Qobuz variant-set registry key)
/// and the source word ("local" | "plex" | "qobuz" | …).
#[derive(Clone, Default)]
pub struct ArtSeed {
    pub url: String,
    pub track_id: u64,
    pub album_id: String,
    pub source: String,
}

static ART_SEED: Mutex<ArtSeed> = Mutex::new(ArtSeed {
    url: String::new(),
    track_id: 0,
    album_id: String::new(),
    source: String::new(),
});

/// The current track's large-art seed (empty url = nothing to re-resolve).
pub(crate) fn art_seed() -> ArtSeed {
    ART_SEED.lock().unwrap().clone()
}

/// `(track_id, progress 0..1)` for the Spectral Ribbon drain (A3,
/// `viz_qt.rs` → `ribbon_qt.rs`): the playback fraction at SECOND
/// granularity — the reference reads Slint's NowPlayingState progress, which
/// is itself second-granular, and the gap-fill exists precisely because it
/// updates ~1 Hz (`shader_underlay.rs:898-899`). Reads the model + the art
/// seed's queue id (the model carries no numeric track id).
pub(crate) fn ribbon_cursor() -> (u64, f32) {
    let guard = MODEL.lock().unwrap();
    let progress = guard
        .as_ref()
        .map(|m| {
            if m.duration_secs > 0 {
                (m.elapsed_secs as f32 / m.duration_secs as f32).clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    drop(guard);
    (art_seed().track_id, progress)
}

/// Attach a resolved artwork path to the current track (artwork pipeline).
pub fn set_artwork_path(path: String) {
    mutate(|m| m.artwork_path = path);
}

/// Publish the size-aware LARGE art sibling (`QbzPlayer.npArtworkPathLarge`).
/// Deliberately NOT a model field: it resolves asynchronously and
/// independently of the meta publish, so it gets the single-property
/// publisher treatment (the `publish_show_context_icon` pattern) — one notify
/// per actual change, no full-model republish per art landing (pulse law:
/// batched, deduped).
pub fn set_artwork_path_large(path: String) {
    static LAST: Mutex<String> = Mutex::new(String::new());
    {
        let mut last = LAST.lock().unwrap();
        if *last == path {
            return;
        }
        *last = path.clone();
    }
    crate::player_bridge::ui(move |mut b| {
        b.as_mut()
            .set_np_artwork_path_large(QString::from(path.as_str()))
    });
}

/// 1 Hz position push from the poll pump. `has_audio` = the engine
/// surfaced a track id (clears the loading spinner).
///
/// Standalone `loading = false` publish for the paths that never reach a
/// position push — the QConnect routed-play REFUSAL case (mixed queue refused
/// by the peer, §12.29): the Slint clears PENDING_PLAY_ID right after
/// `play_on_peer_if_active` (playback.rs:643-650); here the poll loop's peer
/// branch normally clears the spinner via `set_position(has_audio = true)`,
/// but a refused play never primes the peer, so a paused+idle peer would
/// leave the spinner latched.
/// Light the spinner the MOMENT a play is dispatched, before anything is
/// resolved.
///
/// `loading` used to be set only inside the meta publish, i.e. once the track
/// was already known — which leaves the entire resolve/fetch window with no
/// affordance anywhere in the shell. That window is seconds for a Plex part or
/// a cold CMAF session, and the owner read it as a dead click. The clear side
/// already exists (`clear_loading`, plus the position push once audio flows),
/// so this only moves the START earlier.
pub(crate) fn begin_loading() {
    mutate(|m| m.loading = true);
}

pub(crate) fn clear_loading() {
    mutate(|m| m.loading = false);
}

/// A track's meta is on the bar but NOTHING was dispatched — the session
/// restore at startup.
///
/// `set_track` asserts `loading = true` and `playing = true` because its normal
/// caller is a real play, and both are cleared by the audio that follows:
/// `set_position(has_audio = true)` turns the spinner off. A restored track has
/// no dispatch behind it and no audio ahead of it, so nothing ever arrives to
/// clear either flag — the app opened with the play button spinning and the
/// transport claiming to play, forever. Restore is the one caller that has to
/// say so explicitly.
pub(crate) fn mark_restored_idle() {
    mutate(|m| {
        m.loading = false;
        m.playing = false;
    });
}
pub fn set_position(
    elapsed_secs: i32,
    duration_secs: i32,
    playing: bool,
    cache: f32,
    has_audio: bool,
) {
    mutate(|m| {
        m.elapsed_secs = elapsed_secs;
        if duration_secs > 0 {
            m.duration_secs = duration_secs;
        }
        m.playing = playing;
        if has_audio {
            m.loading = false;
        }
        // progress is derived in publish() from elapsed/duration; cache is
        // stored raw for the seekbar's buffer overlay.
        m.cache = cache;
    });
}

/// The seek lock (PARITY-DEBT #15) — `buffer_progress.clamp(0,1)` while a
/// stream is downloading, 1.0 for a fully-available track
/// (`playback.rs:5304`). Kept OUT of `set_position` because the cast path
/// (`cast_qt.rs`) calls that one too and must stay on 1.0 the way the Slint
/// cast publish does (`cast_service.rs:1118`); a signature change there would
/// have silently locked casting to the local stream's fill.
///
/// Deduped: a fully-available track holds 1.0 forever and costs no Qt-thread
/// hop (same pattern as `set_effective_stream`).
pub fn set_seekable_max(seekable_max: f32) {
    let seekable_max = seekable_max.clamp(0.0, 1.0);
    let (changed, snapshot) = with_model(|m| {
        if m.seekable_max == seekable_max {
            return false;
        }
        m.seekable_max = seekable_max;
        true
    });
    if changed {
        publish(&snapshot);
    }
}
