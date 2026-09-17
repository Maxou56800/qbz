//! QbzPlayer — now-playing/transport domain bridge (phase 23 split of the
//! QbzBridge God-object; the pattern is documented in main.rs). Props: the
//! NowPlayingState mirror (np_ prefix) + the playing-row indicator ids +
//! show-context-icon. Invokables: transport + the play/enqueue entry points
//! (album / artist / track / playlist) — one-line forwards into the crate
//! handlers.

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::{QList, QString};

#[cxx_qt::bridge]
pub mod qbz_player {
    extern "C++" {
        include!("cxx-qt-lib/qlist.h");
        include!("cxx-qt-lib/qstring.h");
        type QList_f32 = cxx_qt_lib::QList<f32>;
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // --- Now playing (Slint NowPlayingState; np_ prefix) ------------
        // Fed by the live NowPlayingModel (`now_playing.rs`), which the player
        // poll updates and which supplies empty-state defaults before a track.
        #[qproperty(bool, np_has_track)]
        #[qproperty(QString, np_title)]
        #[qproperty(QString, np_artist)]
        #[qproperty(QString, np_artwork_path)]
        // Size-aware LARGE art sibling (contract 04 §4, D2): resolved per
        // size bucket by request_np_artwork_size for the big immersive slots.
        // "" = no large art resolved — bind with a fallback to
        // np_artwork_path (the Slint `artwork-large : artwork` pattern).
        #[qproperty(QString, np_artwork_path_large)]
        #[qproperty(i32, np_elapsed_secs)]
        #[qproperty(i32, np_duration_secs)]
        #[qproperty(f32, np_progress)]
        #[qproperty(f32, np_cache_progress)]
        // SEEK LOCK (PARITY-DEBT #15, Slint `NowPlayingState.seekable-max`,
        // playback.rs:5304). The fraction of the track the user may seek to:
        // while a stream is still downloading it is `buffer_progress` clamped
        // to 0..1, and a fully-available track (local/offline/complete) is
        // 1.0. The seek bars must clamp the drag/click position to it —
        // `np_cache_progress` is the DECORATIVE overlay of the same fill and
        // must not be used as the limit.
        #[qproperty(f32, np_seekable_max)]
        // Full-track RMS waveform: 512 normalized bins plus the fraction of
        // bins observed so far. The seekbar renders it only when the single
        // app-wide preference is enabled.
        #[qproperty(QList_f32, np_seek_waveform)]
        #[qproperty(f32, np_seek_waveform_analyzed)]
        #[qproperty(bool, np_seek_waveform_complete)]
        #[qproperty(bool, np_playing)]
        #[qproperty(bool, np_loading)]
        #[qproperty(f32, np_volume)]
        #[qproperty(bool, np_muted)]
        #[qproperty(bool, np_shuffle)]
        // 0 off / 1 all / 2 one.
        #[qproperty(i32, np_repeat_mode)]
        // --- Quality stamp (Slint NowPlayingState quality-* block) -------
        // CATALOG max of the track: "hires" | "mp3" | "lossless" | "cd" | "".
        #[qproperty(QString, np_quality_tier)]
        // Exact detail line of the catalog max, e.g. "24-bit / 96 kHz".
        #[qproperty(QString, np_quality_detail)]
        // Legacy alias of np_quality_detail (the pre-contract Qt name) —
        // published from the same value so existing QML keeps working.
        #[qproperty(QString, np_quality_label)]
        // What the OPENED STREAM actually negotiated: "direct" | "plugin" |
        // "disabled" | "unknown". Distinct from the mode LED on purpose - that
        // one is derived from SETTINGS (output_labels.rs), so it says what was
        // ASKED FOR. Only this one can tell "exclusive worked" from "we asked
        // for exclusive", which is what phase B's acceptance turns on.
        #[qproperty(QString, np_bit_perfect_mode)]
        // DELIVERED-vs-catalog state (#590/#638). While `downgraded` is on,
        // the stamp's main line reports the delivered tier/detail and the
        // catalog max moves to the tooltip's "Source" line.
        #[qproperty(bool, np_quality_downgraded)]
        // Delivered detail line while downgraded ("16-bit / 44.1 kHz",
        // "DSD64"); empty when the engine has not reported params yet.
        #[qproperty(QString, np_quality_true_detail)]
        // Delivered tier while downgraded ("hires"|"cd"|"mp3"), else "".
        #[qproperty(QString, np_quality_effective_tier)]
        // WHY the stream is below the catalog max (qbz_models::QualityLimit):
        // 0 none · 1 streaming-quality setting · 2 local output device cap ·
        // 3 cast renderer cap · 4 Qobuz offered nothing higher.
        #[qproperty(i32, np_quality_limit_cause)]
        // A3: the DELIVERED stream params as scalars (0 = not reported yet) —
        // the Spectral Ribbon overlay header reads them
        // (ImmersiveSpectralOverlay.slint:45 parity).
        #[qproperty(i32, np_eff_rate_hz)]
        #[qproperty(i32, np_eff_bits)]
        // --- Output LEDs (settings.rs `output_labels`) -------------------
        // PIPEWIRE|ALSA|JACK|PULS|SYST|AUTO — *_active lights the LED.
        #[qproperty(QString, np_output_backend_label)]
        #[qproperty(bool, np_output_backend_active)]
        // Human-readable backend + selected output name for the LED tooltip,
        // e.g. "ALSA — FiiO K9 Pro". Settings owns device enumeration; the
        // player bridge only mirrors the resolved route for all four NPBs.
        #[qproperty(QString, np_output_backend_tooltip)]
        // DACPASS|BITPERF|EXCL|DIRECT|LOCKED|ROUTED|SHARED|DEFAULT.
        #[qproperty(QString, np_output_mode_label)]
        #[qproperty(bool, np_output_mode_active)]
        // The app's software volume is INERT on this route — the ALSA Direct
        // engine's `set_volume` is a no-op unless the DAC's own mixer is
        // driven (`alsa_hardware_volume`). Derived read-only in
        // `output_labels::volume_locked`; the bars render the volume slider
        // disabled with a tooltip instead of pretending it works. No audio
        // behaviour hangs off this flag — it is a UI mirror only.
        #[qproperty(bool, np_volume_locked)]
        // --- Cast / remote (Qobuz Connect + Chromecast/DLNA) -------------
        // A peer Qobuz Connect renderer owns playback (transport is remote).
        #[qproperty(bool, np_is_remote)]
        // Name of the active renderer / cast target; empty when local.
        #[qproperty(QString, np_cast_target)]
        // A Chromecast/DLNA session is connected — while casting the two
        // output LEDs fuse into a single cast chip.
        #[qproperty(bool, np_cast_active)]
        // "cast" | "dlna".
        #[qproperty(QString, np_cast_protocol)]
        // REMOTE volume lock (Qobuz Connect half of Slint
        // `NowPlayingState.volume-locked`, contract §11.3): the active PEER
        // renderer disallows remote volume control. Distinct from
        // `np_volume_locked` (the read-only ALSA-hw settings derivation in
        // `output_labels.rs`) — the bars' volume-lock expression combines
        // both. Written by `now_playing::set_remote_volume_locked` from the
        // qconnect sink's badge refresh + the facade's disconnect tail.
        #[qproperty(bool, np_remote_volume_locked)]
        #[qproperty(bool, np_remote_volume_pending)]
        // Now-playing track id (playing-row indicator in track lists).
        #[qproperty(QString, np_track_id)]
        // The SAME row's `library.db` id, for OFFLINE rows only, and it is an
        // alias rather than a second identity: a `qobuz_download` row's queue
        // id is the QOBUZ CATALOG id (the offline tier is keyed on it), while
        // every Local Library list draws that row with its library row id. So
        // `np_track_id` alone never matched a downloaded track's row and the
        // playing affordance stayed dark on the one view the track was played
        // from. Empty for every other source — ports Slint's
        // `NowPlayingState.local-track-id` (playback.rs:1978-1986), including
        // its restriction: the hint that feeds it carries an ALBUM key for a
        // Plex row and a server item id for a media-server row, so trusting it
        // for anything but Offline would light the wrong row.
        #[qproperty(QString, np_local_track_id)]
        #[qproperty(QString, np_album)]
        #[qproperty(QString, np_album_id)]
        #[qproperty(QString, np_artist_id)]
        // "Playing from" ORIGIN of the current track (NowPlayingState
        // context-kind / context-id): the container the queue was launched
        // from, republished on every track change. kind is "album" | "artist"
        // | "playlist" | "label"; the song-card layers glyph navigates there.
        #[qproperty(QString, np_context_kind)]
        #[qproperty(QString, np_context_id)]
        // "Show track playing context" pref (Playback settings) — feeds the
        // SongCard layers icon.
        #[qproperty(bool, show_context_icon)]
        // "Show 10-second skip buttons" (Appearance > Player & Visuals) —
        // ui_prefs `show_skip_ten`, mirrored here so TransportControls reads
        // one property on every bar.
        #[qproperty(bool, show_skip_ten)]
        // A-B loop (ab_loop_qt): 0 none / 1 A armed / 2 looping, plus the two
        // marks in whole seconds for the seekbar.
        #[qproperty(i32, ab_state)]
        #[qproperty(i32, ab_start_secs)]
        #[qproperty(i32, ab_end_secs)]
        type QbzPlayer = super::QbzPlayerRust;

        /// Registers this object's Qt-thread hop (Main.qml boots EVERY
        /// domain singleton; only QbzSession.boot also fires crate::on_boot).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzPlayer>);

        /// A big art slot (immersive panels) reports its size in CSS px
        /// (D2, contract 04 §4). Bucketed on the Qobuz variant table: only a
        /// bucket CROSSING re-resolves, the result lands on
        /// `np_artwork_path_large` asynchronously, and a shrink never
        /// re-fetches (the derivative layer downscales the cached file).
        #[qinvokable]
        fn request_np_artwork_size(self: Pin<&mut QbzPlayer>, px: i32);

        // --- Live transport ----------------------------------------------
        #[qinvokable]
        fn toggle_play(self: Pin<&mut QbzPlayer>);
        #[qinvokable]
        fn next(self: Pin<&mut QbzPlayer>);
        #[qinvokable]
        fn previous(self: Pin<&mut QbzPlayer>);
        #[qinvokable]
        fn seek(self: Pin<&mut QbzPlayer>, frac: f32);
        /// Relative seek in whole seconds (the ±10 s buttons); clamps to the
        /// track and to the buffered edge like the seekbars do.
        #[qinvokable]
        fn seek_by(self: Pin<&mut QbzPlayer>, delta_secs: i32);
        /// A-B loop verb: arm A, then B, then clear (see ab_loop_qt).
        #[qinvokable]
        fn ab_mark(self: Pin<&mut QbzPlayer>);
        #[qinvokable]
        fn ab_clear(self: Pin<&mut QbzPlayer>);
        #[qinvokable]
        fn set_volume(self: Pin<&mut QbzPlayer>, volume: f32);
        /// Persist the SETTLED volume (drag-end only — see QbzSlider.released).
        /// `set_volume` drives the engine on every drag tick; this writes the
        /// shared ui_prefs.json once, 1:1 with NowPlayingState.persist-volume
        /// (qbz/src/main.rs:14413-14417).
        #[qinvokable]
        fn persist_volume(self: Pin<&mut QbzPlayer>, fraction: f32);
        #[qinvokable]
        fn toggle_mute(self: Pin<&mut QbzPlayer>);
        #[qinvokable]
        fn toggle_shuffle(self: Pin<&mut QbzPlayer>);
        #[qinvokable]
        fn cycle_repeat(self: Pin<&mut QbzPlayer>);

        /// Album-card click on Home: resolve the album's tracks, enqueue,
        /// and play through the core's resolved path.
        #[qinvokable]
        fn play_album(self: Pin<&mut QbzPlayer>, album_id: QString);

        /// AlbumView header Shuffle.
        #[qinvokable]
        fn play_album_shuffled(self: Pin<&mut QbzPlayer>, album_id: QString);
        /// Artist page album-section split button — a JSON array of album
        /// ids in the section's sort order, queued as one list.
        /// `#[auto_cxx_name]` makes the QML name `QbzPlayer.playAlbums`.
        #[qinvokable]
        fn play_albums(self: Pin<&mut QbzPlayer>, ids_json: QString);
        /// AlbumView row play: the album starting AT this track.
        #[qinvokable]
        fn play_album_from(self: Pin<&mut QbzPlayer>, album_id: QString, track_id: QString);
        /// AlbumView row "Play next" ("next") / "Play later" ("later") /
        /// "Add to queue" ("queue").
        #[qinvokable]
        fn enqueue_album_track(
            self: Pin<&mut QbzPlayer>,
            album_id: QString,
            track_id: QString,
            mode: QString,
        );
        /// AlbumCard ⋯ menu: Play next ("next") / Play later ("later", #442
        /// block tail) / Add to queue ("queue").
        #[qinvokable]
        fn enqueue_album(self: Pin<&mut QbzPlayer>, album_id: QString, mode: QString);
        /// Track-row offline cache actions (offline_cache_qt.rs): download /
        /// remove / refresh the track's offline copy.
        #[qinvokable]
        fn cache_track(self: Pin<&mut QbzPlayer>, track_id: QString);
        #[qinvokable]
        fn uncache_track(self: Pin<&mut QbzPlayer>, track_id: QString);
        #[qinvokable]
        fn recache_track(self: Pin<&mut QbzPlayer>, track_id: QString);
        /// Multi-select bulk actions for Qobuz track listings
        /// (bulk_tracks_qt.rs): ids in visible order + the bar's action id +
        /// the queue context to stamp ("" = none).
        #[qinvokable]
        fn bulk_tracks_action(
            self: Pin<&mut QbzPlayer>,
            ids_json: QString,
            action: QString,
            context_kind: QString,
            context_id: QString,
        );
        /// ArtistView Popular Tracks row play (whole list as the queue).
        #[qinvokable]
        fn play_artist_track(self: Pin<&mut QbzPlayer>, track_id: QString);
        /// ArtistView "Play all" (shuffle=false) / "Shuffle all" (true).
        #[qinvokable]
        fn play_artist_top(self: Pin<&mut QbzPlayer>, shuffle: bool);
        /// ArtistView ⋯ "Play all next" / "Add all to queue" — the top-tracks
        /// queue through the shared insertion funnel. `mode` is "next"
        /// (insert at the cursor) | "later" | anything else (append).
        ///
        /// It takes a MODE because the menu has both rows and only one of
        /// them was reachable: `next-all` was wired to `play_artist_top`,
        /// which REPLACES the queue and starts playing. Asking to queue an
        /// artist next wiped whatever was queued.
        #[qinvokable]
        fn enqueue_artist_top(self: Pin<&mut QbzPlayer>, mode: QString);
        /// Artist-card overlay play (ArtistGridCard): Popular tracks,
        /// falling back to the studio discography (playback.rs play_artist).
        #[qinvokable]
        fn play_artist_card(self: Pin<&mut QbzPlayer>, artist_id: QString);

        /// Track-row click (Library): play the track as a 1-element queue.
        #[qinvokable]
        fn play_track(self: Pin<&mut QbzPlayer>, track_id: QString);
        /// Play one track KNOWING where the row came from ("qobuz" | "local" |
        /// "plex"; empty = qobuz). Use this from any rail whose rows can be
        /// non-Qobuz — `play_track` alone only has an id, and an id is not
        /// enough to tell a library rowid from a Qobuz track id.
        #[qinvokable]
        fn play_track_from(self: Pin<&mut QbzPlayer>, track_id: QString, source: QString);
        /// Library track context menus: Play next ("next") / Play later
        /// ("later") / Add to queue ("queue") on a single feed track.
        #[qinvokable]
        fn enqueue_track(self: Pin<&mut QbzPlayer>, track_id: QString, mode: QString);

        /// Card-level playlist actions (LibPlaylistCard overlay + menu).
        #[qinvokable]
        fn play_playlist_by_id(self: Pin<&mut QbzPlayer>, playlist_id: QString);
        #[qinvokable]
        fn enqueue_playlist_by_id(self: Pin<&mut QbzPlayer>, playlist_id: QString, mode: QString);
    }

    impl cxx_qt::Threading for QbzPlayer {}
}

// `self::` disambiguates the bridge mod from the `qbz-player` extern crate
// (added for the QConnect engine, contract §1.1) — E0659 otherwise.
use self::qbz_player::QbzPlayer;

/// Rust side of the player bridge (plain storage, phase-1 pattern).
pub struct QbzPlayerRust {
    np_has_track: bool,
    np_title: QString,
    np_artist: QString,
    np_artwork_path: QString,
    np_artwork_path_large: QString,
    np_elapsed_secs: i32,
    np_duration_secs: i32,
    np_progress: f32,
    np_cache_progress: f32,
    np_seekable_max: f32,
    np_seek_waveform: QList<f32>,
    np_seek_waveform_analyzed: f32,
    np_seek_waveform_complete: bool,
    np_playing: bool,
    np_loading: bool,
    np_volume: f32,
    np_muted: bool,
    np_shuffle: bool,
    np_repeat_mode: i32,
    np_quality_tier: QString,
    np_quality_detail: QString,
    np_quality_label: QString,
    np_bit_perfect_mode: QString,
    np_quality_downgraded: bool,
    np_quality_true_detail: QString,
    np_quality_effective_tier: QString,
    np_quality_limit_cause: i32,
    np_eff_rate_hz: i32,
    np_eff_bits: i32,
    np_output_backend_label: QString,
    np_output_backend_active: bool,
    np_output_backend_tooltip: QString,
    np_output_mode_label: QString,
    np_output_mode_active: bool,
    np_volume_locked: bool,
    np_is_remote: bool,
    np_cast_target: QString,
    np_cast_active: bool,
    np_cast_protocol: QString,
    np_remote_volume_locked: bool,
    np_remote_volume_pending: bool,
    np_track_id: QString,
    np_local_track_id: QString,
    np_album: QString,
    np_album_id: QString,
    np_artist_id: QString,
    np_context_kind: QString,
    np_context_id: QString,
    show_context_icon: bool,
    show_skip_ten: bool,
    ab_state: i32,
    ab_start_secs: i32,
    ab_end_secs: i32,
}

impl Default for QbzPlayerRust {
    fn default() -> Self {
        Self {
            // A derive would zero these — the model's sane defaults instead.
            np_volume: 1.0,
            np_quality_tier: QString::from("cd"),
            np_has_track: false,
            np_title: QString::default(),
            np_artist: QString::default(),
            np_artwork_path: QString::default(),
            np_artwork_path_large: QString::default(),
            np_elapsed_secs: 0,
            np_duration_secs: 0,
            np_progress: 0.0,
            np_cache_progress: 0.0,
            // Seek freely until a stream says otherwise — the Slint default
            // (`NowPlayingState.seekable-max: 1.0`). Zero here would lock the
            // bar shut before the first track ever opens.
            np_seekable_max: 1.0,
            np_seek_waveform: QList::default(),
            np_seek_waveform_analyzed: 0.0,
            np_seek_waveform_complete: false,
            np_playing: false,
            np_loading: false,
            np_muted: false,
            np_shuffle: false,
            np_repeat_mode: 0,
            np_quality_detail: QString::default(),
            np_quality_label: QString::default(),
            np_bit_perfect_mode: QString::default(),
            np_quality_downgraded: false,
            np_quality_true_detail: QString::default(),
            np_quality_effective_tier: QString::default(),
            np_quality_limit_cause: 0,
            np_eff_rate_hz: 0,
            np_eff_bits: 0,
            // The Slint NowPlayingState defaults (state.slint) — an unlit
            // "SYST / DEFAULT" pair until the first settings snapshot.
            np_output_backend_label: QString::from("SYST"),
            np_output_backend_active: false,
            np_output_backend_tooltip: QString::from("System default"),
            np_output_mode_label: QString::from("DEFAULT"),
            np_output_mode_active: false,
            // Optimistic default: the slider stays live until the first
            // settings read proves the route is an inert ALSA-direct one.
            np_volume_locked: false,
            np_is_remote: false,
            np_cast_target: QString::default(),
            np_cast_active: false,
            np_cast_protocol: QString::default(),
            np_remote_volume_locked: false,
            np_remote_volume_pending: false,
            np_track_id: QString::default(),
            np_local_track_id: QString::default(),
            np_album: QString::default(),
            np_album_id: QString::default(),
            np_artist_id: QString::default(),
            np_context_kind: QString::default(),
            np_context_id: QString::default(),
            // Seed only — this runs when the QML engine constructs the
            // singleton, which can be BEFORE the playback-preferences store is
            // open (then it reads false). `now_playing::publish_show_context_icon`
            // re-publishes it on shell entry and on the Settings toggle.
            show_context_icon: crate::settings_qt::show_context_icon(),
            show_skip_ten: crate::settings_qt::pref_bool("show_skip_ten", false),
            ab_state: 0,
            ab_start_secs: 0,
            ab_end_secs: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop (bridges/mod.rs pattern)
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzPlayer>> = OnceLock::new();

extern "C" {
    fn qbz_seek_waveform_register_qml_type();
}

pub(crate) fn register_seek_waveform_qml_item() {
    // SAFETY: no arguments; the C++ side guards duplicate registration. This
    // explicit reference keeps the hand-written scenegraph item linked from
    // the static archive on every platform.
    unsafe { qbz_seek_waveform_register_qml_type() };
}

/// Queue a player-bridge mutation onto the Qt event loop (no-op before
/// boot registers the thread).
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzPlayer>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

impl qbz_player::QbzPlayer {
    pub fn boot(self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] player Qt thread already registered");
        }
    }

    pub fn request_np_artwork_size(self: Pin<&mut Self>, px: i32) {
        crate::artwork_qt::request_now_playing_art(px);
    }

    pub fn toggle_play(self: Pin<&mut Self>) {
        crate::transport_toggle_play();
    }

    pub fn next(self: Pin<&mut Self>) {
        crate::transport_next();
    }

    pub fn previous(self: Pin<&mut Self>) {
        crate::transport_previous();
    }

    pub fn seek(self: Pin<&mut Self>, frac: f32) {
        crate::transport_seek(frac);
    }

    pub fn seek_by(self: Pin<&mut Self>, delta_secs: i32) {
        crate::transport_seek_by(delta_secs);
    }

    pub fn ab_mark(self: Pin<&mut Self>) {
        crate::ab_loop_mark();
    }

    pub fn ab_clear(self: Pin<&mut Self>) {
        crate::ab_loop_clear();
    }

    pub fn persist_volume(self: Pin<&mut Self>, fraction: f32) {
        if *self.np_is_remote() {
            return;
        }
        crate::settings_qt::save_pref("volume", serde_json::json!(fraction.clamp(0.0, 1.0)));
    }

    pub fn set_volume(self: Pin<&mut Self>, volume: f32) {
        crate::transport_set_volume(volume);
    }

    pub fn toggle_mute(self: Pin<&mut Self>) {
        crate::transport_toggle_mute();
    }

    pub fn toggle_shuffle(self: Pin<&mut Self>) {
        crate::transport_toggle_shuffle();
    }

    pub fn cycle_repeat(self: Pin<&mut Self>) {
        crate::transport_cycle_repeat();
    }

    pub fn play_album(self: Pin<&mut Self>, album_id: QString) {
        crate::play_album(album_id.to_string());
    }

    pub fn play_albums(self: Pin<&mut Self>, ids_json: QString) {
        crate::play_albums(ids_json.to_string());
    }

    pub fn play_album_shuffled(self: Pin<&mut Self>, album_id: QString) {
        crate::play_album_shuffled(album_id.to_string());
    }

    pub fn play_album_from(self: Pin<&mut Self>, album_id: QString, track_id: QString) {
        if let Ok(tid) = track_id.to_string().parse::<u64>() {
            crate::play_album_from_track(album_id.to_string(), tid);
        }
    }

    pub fn enqueue_album_track(
        self: Pin<&mut Self>,
        album_id: QString,
        track_id: QString,
        mode: QString,
    ) {
        if let Ok(tid) = track_id.to_string().parse::<u64>() {
            crate::enqueue_album_track(album_id.to_string(), tid, mode.to_string());
        }
    }

    pub fn enqueue_album(self: Pin<&mut Self>, album_id: QString, mode: QString) {
        crate::enqueue_album(album_id.to_string(), mode.to_string());
    }

    pub fn cache_track(self: Pin<&mut Self>, track_id: QString) {
        if let Ok(id) = track_id.to_string().parse::<u64>() {
            crate::offline_cache_qt::cache_track(id);
        }
    }
    pub fn uncache_track(self: Pin<&mut Self>, track_id: QString) {
        if let Ok(id) = track_id.to_string().parse::<u64>() {
            crate::offline_cache_qt::remove_cached(id);
        }
    }
    pub fn recache_track(self: Pin<&mut Self>, track_id: QString) {
        if let Ok(id) = track_id.to_string().parse::<u64>() {
            crate::offline_cache_qt::refresh_cached(id);
        }
    }

    pub fn bulk_tracks_action(
        self: Pin<&mut Self>,
        ids_json: QString,
        action: QString,
        context_kind: QString,
        context_id: QString,
    ) {
        crate::bulk_tracks_qt::run(
            ids_json.to_string(),
            action.to_string(),
            context_kind.to_string(),
            context_id.to_string(),
        );
    }

    pub fn play_artist_track(self: Pin<&mut Self>, track_id: QString) {
        if let Ok(tid) = track_id.to_string().parse::<u64>() {
            crate::play_artist_track(tid);
        }
    }

    pub fn play_artist_top(self: Pin<&mut Self>, shuffle: bool) {
        crate::play_artist_top(shuffle);
    }

    pub fn enqueue_artist_top(self: Pin<&mut Self>, mode: QString) {
        crate::enqueue_artist_top(mode.to_string());
    }

    pub fn play_artist_card(self: Pin<&mut Self>, artist_id: QString) {
        crate::play_artist_card(artist_id.to_string());
    }

    pub fn play_track(self: Pin<&mut Self>, track_id: QString) {
        if let Ok(id) = track_id.to_string().parse::<u64>() {
            crate::play_track(id);
        }
    }

    pub fn play_track_from(self: Pin<&mut Self>, track_id: QString, source: QString) {
        if let Ok(id) = track_id.to_string().parse::<u64>() {
            crate::play_track_from(id, source.to_string());
        }
    }

    pub fn enqueue_track(self: Pin<&mut Self>, track_id: QString, mode: QString) {
        if let Ok(id) = track_id.to_string().parse::<u64>() {
            crate::enqueue_track(id, mode.to_string());
        }
    }

    pub fn play_playlist_by_id(self: Pin<&mut Self>, playlist_id: QString) {
        if let Ok(pid) = playlist_id.to_string().parse::<u64>() {
            crate::play_playlist_by_id(pid);
        }
    }

    pub fn enqueue_playlist_by_id(self: Pin<&mut Self>, playlist_id: QString, mode: QString) {
        if let Ok(pid) = playlist_id.to_string().parse::<u64>() {
            crate::enqueue_playlist_by_id(pid, mode.to_string());
        }
    }
}
