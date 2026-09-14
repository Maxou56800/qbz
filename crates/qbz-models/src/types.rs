//! Core API types for QBZ
//!
//! This module contains all shared data types used across the application:
//! - Media types: Track, Album, Artist, Playlist
//! - Quality/streaming types
//! - Search and favorites types
//! - Image and metadata types

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ============ Dynamic-suggest (DailyQ/WeeklyQ) ============

/// A seed track resolved for the `/dynamic/suggest` `track_to_analysed`
/// payload (DailyQ / WeeklyQ). Field names match the Qobuz wire shape
/// exactly; `0` marks an unknown id (mirrors Tauri's `?? 0`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackToAnalyse {
    pub track_id: u64,
    pub artist_id: u64,
    pub genre_id: u64,
    pub label_id: u64,
}

// ============ Quality Types ============

/// Audio quality format IDs (matches Qobuz API format IDs)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u32)]
pub enum Quality {
    Mp3 = 5,
    Lossless = 6,    // 16-bit/44.1kHz (CD Quality)
    HiRes = 7,       // 24-bit/≤96kHz
    UltraHiRes = 27, // 24-bit/>96kHz
}

impl Quality {
    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            5 => Some(Quality::Mp3),
            6 => Some(Quality::Lossless),
            7 => Some(Quality::HiRes),
            27 => Some(Quality::UltraHiRes),
            _ => None,
        }
    }

    pub fn id(&self) -> u32 {
        *self as u32
    }

    pub fn label(&self) -> &'static str {
        match self {
            Quality::Mp3 => "MP3 320kbps",
            Quality::Lossless => "FLAC 16-bit/44.1kHz",
            Quality::HiRes => "FLAC 24-bit/≤96kHz",
            Quality::UltraHiRes => "FLAC 24-bit/>96kHz",
        }
    }

    /// Quality levels in descending order for fallback
    pub fn fallback_order() -> &'static [Quality] {
        &[
            Quality::UltraHiRes,
            Quality::HiRes,
            Quality::Lossless,
            Quality::Mp3,
        ]
    }

    /// Returns the next lower quality level, or None if already at the lowest (Mp3).
    /// Used for CDN fallback when a quality level consistently fails.
    pub fn lower(&self) -> Option<Quality> {
        match self {
            Quality::UltraHiRes => Some(Quality::HiRes),
            Quality::HiRes => Some(Quality::Lossless),
            Quality::Lossless => Some(Quality::Mp3),
            Quality::Mp3 => None,
        }
    }

    /// The lower of two tiers. Implementable as plain `min` because the
    /// derived `Ord` on `Quality` is tier-correct: the Qobuz format-id
    /// discriminants (5 Mp3 < 6 Lossless < 7 HiRes < 27 UltraHiRes) ascend
    /// with tier. Used to clamp a requested tier against a cap (#638).
    pub fn min_tier(a: Quality, b: Quality) -> Quality {
        a.min(b)
    }
}

impl Default for Quality {
    fn default() -> Self {
        Quality::Lossless
    }
}

/// Why a delivered stream is (or may be) below the track's catalog maximum.
/// Shared by the local badge, the local device cap, and the cast surfaces
/// (#638 fixes 1-4). Mirrored to Slint as a plain `int` property carrying the
/// same discriminant — no string enum crosses the FFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityLimit {
    /// No constraint identified (or no downgrade).
    #[default]
    None = 0,
    /// The user's streaming-quality preference capped the request.
    Preference = 1,
    /// The local output device's cap lowered the request (fix 3).
    /// NEVER applicable while casting — the local DAC is not in a cast's
    /// signal path (precedence rule, owner decision 2026-07-20).
    LocalDeviceCap = 2,
    /// The manual per-renderer cap lowered the request (fix 4). Cast only.
    RendererCap = 3,
    /// Qobuz did not offer a higher tier for this track.
    CatalogAvailability = 4,
}

// ============ User Session ============

/// What the account is entitled to, straight from
/// `user.credential.parameters` of the `/user/login` payload (wire names,
/// verified against Qobuz's own embedded fixture — see
/// `qbz-nix-docs/offline-mode/tauri-review-2026-06-09/10-subscription-trial-offline-gating.md`
/// §1.2). An account without an active subscription ("Qobuz member") has
/// no `parameters` at all and therefore every flag `false`; Qobuz still
/// serves it favorites, playlists, purchases and 30-second previews.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlements {
    #[serde(default)]
    pub lossy_streaming: bool,
    #[serde(default)]
    pub lossless_streaming: bool,
    #[serde(default)]
    pub hires_streaming: bool,
    #[serde(default)]
    pub hires_purchases_streaming: bool,
    #[serde(default)]
    pub offline_streaming: bool,
    #[serde(default)]
    pub mobile_streaming: bool,
}

impl Entitlements {
    /// No streaming entitlement of any kind: a Qobuz member without a
    /// subscription. Purchases and previews still work; full-length
    /// catalog playback and offline do not.
    pub fn is_member_only(&self) -> bool {
        !(self.lossy_streaming || self.lossless_streaming || self.hires_streaming)
    }
}

/// User credentials and session info
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserSession {
    pub user_auth_token: String,
    pub user_id: u64,
    pub email: String,
    pub display_name: String,
    pub subscription_label: String,
    #[serde(default)]
    pub subscription_valid_until: Option<String>,
    /// Entitlement flags from `credential.parameters`. `serde(default)`
    /// keeps persisted pre-member-mode sessions loadable (all `false`,
    /// which the next real login corrects).
    #[serde(default)]
    pub entitlements: Entitlements,
    /// `user.subscription.end_date` (`YYYY-MM-DD`), when Qobuz sends the
    /// `subscription` object at all. Informational: the entitlement flags
    /// are the server's verdict, not this date (the embedded fixture has a
    /// past `end_date` with every flag still `true`).
    #[serde(default)]
    pub subscription_end_date: Option<String>,
    /// `user.subscription.offer` (e.g. "studio", "sublime"), when present.
    #[serde(default)]
    pub subscription_offer: Option<String>,
    /// Account territory (ISO 3166-1 alpha-2, e.g. "FR") from the login
    /// response. `serde(default)` keeps pre-v10 persisted sessions loadable.
    #[serde(default)]
    pub country_code: Option<String>,
    /// Account language (ISO 639-1, e.g. "fr") from the login response —
    /// the default target for lyrics translation ("Auto").
    #[serde(default)]
    pub language_code: Option<String>,
}

// ============ Stream Types ============

/// Stream URL response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamUrl {
    pub url: String,
    pub format_id: u32,
    pub mime_type: String,
    pub sampling_rate: f64,
    pub bit_depth: Option<u32>,
    pub track_id: u64,
    pub restrictions: Vec<StreamRestriction>,
    /// `true` when Qobuz served a 30-second preview instead of the track
    /// (an account without the streaming entitlement, or a track that is
    /// not streamable in the territory). The URL still plays; the player
    /// must say so and must not count it as a listen.
    #[serde(default)]
    pub sample: bool,
}

impl StreamUrl {
    /// Check if the stream has restrictions that prevent playback
    pub fn has_restrictions(&self) -> bool {
        self.restrictions.iter().any(|r| {
            r.code == "FormatRestrictedByFormatAvailability"
                || r.code == "SampleRestrictedByRightHolders"
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamRestriction {
    pub code: String,
}

// ============ External streaming (Cast / DLNA) ============

/// Resolved audio quality actually delivered for an external stream, in the
/// kHz convention used across the catalog and [`StreamUrl`]. Surfaced so the
/// UI can show the REAL quality of a cast stream, which can fall back below
/// the requested tier (HiRes -> Lossless -> Mp3) without the user knowing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamQualityInfo {
    /// Qobuz format id: 5=MP3, 6=Lossless, 7=HiRes, 27=UltraHiRes.
    pub format_id: u32,
    /// Sampling rate in kHz (e.g. 96.0, 192.0), when known.
    pub sampling_rate_khz: Option<f64>,
    /// Bit depth (16 / 24), when known.
    pub bit_depth: Option<u32>,
}

impl StreamQualityInfo {
    /// Build from a raw sampling-rate value whose unit may be kHz or Hz
    /// depending on the Qobuz endpoint (`get_stream_url` reports kHz as f64,
    /// `file/url` reports an integer that has been observed as kHz). Normalize
    /// to kHz robustly: any real audio rate is < 1000 kHz and >= 8000 Hz, so a
    /// value >= 1000 is Hz and gets divided. Zero/negative -> unknown.
    pub fn from_raw(format_id: u32, raw_rate: Option<f64>, bit_depth: Option<u32>) -> Self {
        let sampling_rate_khz = raw_rate.and_then(|r| {
            if r <= 0.0 {
                None
            } else if r >= 1000.0 {
                Some(r / 1000.0)
            } else {
                Some(r)
            }
        });
        Self {
            format_id,
            sampling_rate_khz,
            bit_depth,
        }
    }

    /// The `Quality` tier this format id maps to, if recognized.
    pub fn quality(&self) -> Option<Quality> {
        Quality::from_id(self.format_id)
    }

    /// Coarse tier label like "FLAC 24-bit/>96kHz" (from the format id).
    pub fn tier_label(&self) -> &'static str {
        self.quality().map(|q| q.label()).unwrap_or("Unknown")
    }
}

/// Measured stream parameters read from the head of an audio buffer
/// (FLAC STREAMINFO). `sample_rate` is in Hz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioParams {
    pub sample_rate: u32,
    pub bits_per_sample: u32,
    pub channels: u16,
}

/// Parse a FLAC STREAMINFO block from the head of a stream. Returns `None`
/// for non-FLAC or short buffers — never guesses defaults (callers that need
/// a fallback keep their own). Bit math hoisted verbatim from the proven
/// QConnect remote-stream probe (`qbz::remote_stream`), shared so the cast
/// path can measure the bytes it actually serves (#638 fix 1).
pub fn probe_streaminfo(bytes: &[u8]) -> Option<AudioParams> {
    if bytes.len() >= 26 && bytes.starts_with(b"fLaC") {
        let sample_rate =
            ((bytes[18] as u32) << 12) | ((bytes[19] as u32) << 4) | ((bytes[20] as u32) >> 4);
        let channels = ((bytes[20] >> 1) & 0x07) + 1;
        let bit_depth = ((bytes[20] & 0x01) << 4) | ((bytes[21] >> 4) & 0x0F);
        Some(AudioParams {
            sample_rate,
            bits_per_sample: (bit_depth + 1) as u32,
            channels: channels as u16,
        })
    } else {
        None
    }
}

/// Where the bytes for an external/cast track were resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetOrigin {
    Network,
    Cache,
    Offline,
}

/// A fully-materialized audio asset to hand to an external renderer
/// (Chromecast / DLNA) through the local media server. Carries the raw bytes
/// VERBATIM (no transcode), the MIME to advertise, and the quality actually
/// resolved so the UI can display it. Casting bypasses the local audio
/// backend, so this is the only place the delivered quality is known.
#[derive(Clone)]
pub struct ExternalStreamAsset {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub quality: StreamQualityInfo,
    /// Track duration in seconds, when known by the resolver.
    pub duration_secs: Option<f64>,
    pub origin: AssetOrigin,
}

impl std::fmt::Debug for ExternalStreamAsset {
    // Don't dump the whole byte vec into logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalStreamAsset")
            .field("bytes", &format_args!("{} bytes", self.bytes.len()))
            .field("content_type", &self.content_type)
            .field("quality", &self.quality)
            .field("duration_secs", &self.duration_secs)
            .field("origin", &self.origin)
            .finish()
    }
}

// ============ CMAF Stream Types ============

/// Response from POST /api.json/0.2/session/start
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartResponse {
    pub session_id: String,
    pub expires_at: u64,
    #[serde(default)]
    pub infos: Option<String>,
}

/// Response from GET /api.json/0.2/file/url (CMAF segmented streaming)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackFileUrl {
    #[serde(default)]
    pub url_template: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub n_segments: u8,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub sampling_rate: Option<u32>,
    #[serde(default)]
    pub bit_depth: Option<u32>,
    #[serde(default)]
    pub bits_depth: Option<u32>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub n_samples: Option<u64>,
    #[serde(default)]
    pub format_id: Option<u32>,
    #[serde(default)]
    pub track_id: Option<u64>,
    #[serde(default)]
    pub restrictions: Vec<StreamRestriction>,
}

// ============ Image Types ============

/// Pixel size each `ImageSet` variant serves, ascending: small, thumbnail,
/// large, extralarge, mega. This is the Qobuz CDN CONVENTION — the API does
/// not state variant dimensions in-repo (contract
/// `2026-08-15-immersive-completion` 00 §8: verify against live responses
/// before retuning). THE table lives here and only here: `ImageSet::for_px`
/// and the size bucketing (`ImageSet::bucket_for_px`, used by the Qt
/// frontend's responsive-art request path) both read it, so tuning either
/// side is ONE edit.
pub const IMAGE_VARIANT_PX: [u32; 5] = [50, 150, 300, 600, 3000];

/// Image set with multiple resolutions
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageSet {
    pub small: Option<String>,
    pub thumbnail: Option<String>,
    pub large: Option<String>,
    pub extralarge: Option<String>,
    pub mega: Option<String>,
    pub back: Option<String>,
}

impl ImageSet {
    pub fn best(&self) -> Option<&String> {
        self.mega
            .as_ref()
            .or(self.extralarge.as_ref())
            .or(self.large.as_ref())
            .or(self.thumbnail.as_ref())
            .or(self.small.as_ref())
    }

    /// The smallest variant whose pixel size (per [`IMAGE_VARIANT_PX`])
    /// covers `px` — the size-aware pick for a surface that knows its slot
    /// size, so a 72px row never fetches a ~3000px mega and a big slot never
    /// settles for a 150px thumbnail. A missing variant keeps the scan
    /// moving UP (a bigger variant still covers the slot); when nothing is
    /// big enough (or the set is partial — Discover returns only
    /// small/thumbnail/large) the fallbacks are `best()` and then
    /// `smallest()` order, so this never returns WORSE than what a bare
    /// `best()` call would have served.
    pub fn for_px(&self, px: u32) -> Option<&String> {
        let variants = [
            (&self.small, IMAGE_VARIANT_PX[0]),
            (&self.thumbnail, IMAGE_VARIANT_PX[1]),
            (&self.large, IMAGE_VARIANT_PX[2]),
            (&self.extralarge, IMAGE_VARIANT_PX[3]),
            (&self.mega, IMAGE_VARIANT_PX[4]),
        ];
        for (slot, slot_px) in variants {
            if slot_px >= px {
                if let Some(url) = slot.as_ref() {
                    return Some(url);
                }
            }
        }
        self.best().or_else(|| self.smallest())
    }

    /// The size bucket a slot of `px` pixels resolves to: the smallest
    /// [`IMAGE_VARIANT_PX`] entry that covers it, or the mega entry when none
    /// does. Callers that re-resolve art on window resize compare BUCKETS, not
    /// pixels, so a resize drag costs at most one re-request per tier crossed
    /// instead of one per pixel.
    pub fn bucket_for_px(px: u32) -> u32 {
        IMAGE_VARIANT_PX
            .iter()
            .copied()
            .find(|&entry| entry >= px)
            .unwrap_or(IMAGE_VARIANT_PX[IMAGE_VARIANT_PX.len() - 1])
    }

    /// The smallest available variant — for list-row thumbnails, where
    /// `best()` (mega/large) would needlessly download huge images.
    pub fn smallest(&self) -> Option<&String> {
        self.small
            .as_ref()
            .or(self.thumbnail.as_ref())
            .or(self.large.as_ref())
            .or(self.extralarge.as_ref())
            .or(self.mega.as_ref())
    }
}

/// Rewrite a sized Qobuz CDN cover url to the variant tier covering `px`.
///
/// Qobuz covers carry the variant's pixel size as the `_<px>.<ext>` suffix
/// (`https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_50.jpg`),
/// with the sizes of [`IMAGE_VARIANT_PX`] — EXCEPT the mega tier, whose
/// suffix is the literal `_max` (the size it serves is the album master's,
/// 1400px and up). The queue, `recently_played.json`
/// and the persisted session store WHATEVER variant the building surface
/// picked, so a restored session can carry the 50px `small` into the
/// now-playing feed, MPRIS and Discord — measured 2026-08-15 on the owner's
/// cache: 50x50 JPEGs downloaded by the Qt now-playing art feed, pixelated
/// art on every big surface.
///
/// The rewrite targets the smallest table entry covering `px`
/// ([`ImageSet::bucket_for_px`]), so any surface holding ANY sized Qobuz
/// cover can serve itself the tier it needs without the original
/// `ImageSet`. Returns `None` — leaving the url to the caller unchanged —
/// for non-Qobuz urls, unsized covers, and suffixes the table does not know
/// (a stray `_<digits>` in an id is never mistaken for a size).
pub fn qobuz_cover_at_px(url: &str, px: u32) -> Option<String> {
    if !url.contains("/images/covers/") {
        return None;
    }
    let (stem, ext) = url.rsplit_once('.')?;
    if ext.len() > 4 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let (head, size) = stem.rsplit_once('_')?;
    // The mega tier's suffix is the LITERAL `_max`, not `_3000`: measured
    // against the live CDN 2026-08-15, `_3000.jpg` is a 404 on albums whose
    // mega exists (`_max.jpg` serves it, at whatever size that album's
    // master is — 1400px and up), and the API advertises mega urls with the
    // `_max` suffix. `3000` stays the tier's pixel CONVENTION in
    // [`IMAGE_VARIANT_PX`]; only the wire suffix is `max`.
    if size != "max" && !IMAGE_VARIANT_PX.contains(&size.parse::<u32>().ok()?) {
        return None;
    }
    let bucket = ImageSet::bucket_for_px(px);
    if bucket == IMAGE_VARIANT_PX[IMAGE_VARIANT_PX.len() - 1] {
        Some(format!("{head}_max.{ext}"))
    } else {
        Some(format!("{head}_{bucket}.{ext}"))
    }
}

// ============ Core Media Types ============

/// Track model
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Track {
    #[serde(default)]
    pub id: u64,
    /// Favourites-list rows only (unix seconds); see `Album::favorited_at`.
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub favorited_at: Option<i64>,
    /// Store flag from the catalog (`purchasable`): `Some(false)` is the API
    /// saying this release is not sold on Qobuz; absence is unknown, not no.
    #[serde(default)]
    pub purchasable: Option<bool>,
    #[serde(default)]
    pub title: String,
    /// Subtitle/edition info from Qobuz (e.g. "Player's Ball Mix",
    /// "Nine Inch Noize Version", "Remastered 2024"). Frontend renders
    /// it parenthesized after the title so remix and reissue albums are
    /// distinguishable from originals (issue #360).
    pub version: Option<String>,
    /// Classical "work" the track belongs to (e.g. "Symphony No. 9 in D minor,
    /// Op. 125"). Qobuz returns it on the track object (always present in the
    /// envelope, `null` for non-classical catalog). Drives the per-work section
    /// headers on the album view, mirroring the official Qobuz player (PR #536).
    pub work: Option<String>,
    pub isrc: Option<String>,
    /// `audio_info` on the wire: replaygain gain/peak (always present in the
    /// captured samples). Absent on older cached payloads → `None`.
    #[serde(default)]
    pub audio_info: Option<DiscoverAudioInfo>,
    #[serde(default)]
    pub duration: u32,
    #[serde(default)]
    pub track_number: u32,
    pub media_number: Option<u32>,
    pub performer: Option<Artist>,
    pub album: Option<AlbumSummary>,
    #[serde(default)]
    pub hires: bool,
    #[serde(default)]
    pub hires_streamable: bool,
    pub maximum_sampling_rate: Option<f64>,
    pub maximum_bit_depth: Option<u32>,
    /// Whether Qobuz will stream this track today.
    ///
    /// `None` means the endpoint did not report availability — it is **NOT** a
    /// claim of unavailability. Only `Some(false)` marks a track as pulled from
    /// the catalogue. The distinction is load-bearing: this field used to be a
    /// plain `bool` with a bare `#[serde(default)]`, and that collapses an
    /// ABSENT key into `false`, so every track returned by an endpoint that
    /// omits it came back marked unavailable. That was harmless while nothing
    /// rendered the flag; it stops being harmless the moment the flag greys a
    /// row out and keeps it out of the queue, where a single terse endpoint
    /// would silently take out an entire view.
    ///
    /// `/album/get` does send the key (captured 2026-08-17, album
    /// `0886443985094`, where track 1 is `false` while the ALBUM around it is
    /// `true` — the album-level flag can never stand in for this one).
    /// `/playlist/get?extra=tracks` has never been captured, and playlists are
    /// where dead tracks show up most; that unverified endpoint is exactly what
    /// this `Option` guards against.
    ///
    /// Never read the field directly — go through [`Track::is_streamable`],
    /// which is the one place absence is interpreted.
    ///
    /// Mirrors [`Album::streamable`], which has been `Option<bool>` all along
    /// for the same reasons; the two shapes are now consistent.
    #[serde(default)]
    pub streamable: Option<bool>,
    /// Unix seconds at which streaming rights START for this track
    /// (`/album/get` sends it on every item: a PAST value on live tracks,
    /// `null` on a pulled one, a FUTURE value on a pre-release). Read through
    /// [`Track::is_upcoming`], which is what tells "not yet" from "no longer".
    #[serde(default)]
    pub streamable_at: Option<i64>,
    /// Per-track streaming release date (ISO `YYYY-MM-DD`), the same shape as
    /// [`Album::release_date_stream`]. Fallback for [`Track::is_upcoming`]
    /// when `streamable_at` is absent.
    #[serde(default)]
    pub release_date_stream: Option<String>,
    #[serde(default)]
    pub parental_warning: bool,
    /// Playlist-specific: ID within the playlist (for removal)
    pub playlist_track_id: Option<u64>,
    /// Performers/credits string (format: "Name, Role - Name, Role")
    pub performers: Option<String>,
    /// Composer information
    pub composer: Option<Artist>,
    /// Copyright information
    pub copyright: Option<String>,
}

impl Track {
    /// The ONE place an absent `streamable` is interpreted: **absent means
    /// available**.
    ///
    /// The asymmetry is deliberate, and it is the whole reason the field is an
    /// `Option` rather than a `bool` with a convenient default.
    ///
    /// Refusing to play a track because an endpoint was terse is strictly worse
    /// than offering one that turns out to be dead. The second case is already
    /// caught downstream by the reactive path — the play fails, the error is
    /// recognised as terminal (`is_terminal_unavailable`), and the bounded
    /// auto-skip moves on — so it costs one failed round trip and one toast,
    /// and the user hears the next song. The first case has no recovery at all:
    /// the user is shown a greyed, unplayable catalogue, no request is ever
    /// made, nothing fails loudly, and there is no signal anywhere that would
    /// tell them (or us) that the data was merely missing rather than the music
    /// gone. On a music player, silent refusal is the unrecoverable failure and
    /// a wasted request is the cheap one.
    ///
    /// Call this instead of touching `streamable` directly. A hand-written
    /// `unwrap_or` at a call site is how the two halves of this rule drift
    /// apart, and the drift is invisible until a whole view goes grey.
    pub fn is_streamable(&self) -> bool {
        self.streamable.unwrap_or(true)
    }

    /// Whether this track is a PRE-RELEASE: not streamable today because its
    /// streaming rights have not started yet, as opposed to a track Qobuz
    /// pulled. Only meaningful alongside `!is_streamable()`; a streamable
    /// track is never "upcoming". See [`upcoming_from`] for the rule.
    pub fn is_upcoming(&self) -> bool {
        !self.is_streamable()
            && upcoming_from(
                self.streamable_at,
                self.release_date_stream.as_deref(),
                chrono::Utc::now(),
            )
    }
}

/// The ONE rule that tells "not yet available" from "no longer available":
/// a FUTURE `streamable_at` (Unix seconds) wins; without it, a FUTURE
/// `release_date_stream` (`YYYY-MM-DD`, compared by calendar day in UTC).
/// Anything absent, malformed, or in the past reads as NOT upcoming, so a
/// terse endpoint keeps the existing "no longer available" wording rather
/// than promising a release that never comes.
pub fn upcoming_now(streamable_at: Option<i64>, release_date_stream: Option<&str>) -> bool {
    upcoming_from(streamable_at, release_date_stream, chrono::Utc::now())
}

/// [`upcoming_now`] against an explicit clock (tests).
pub fn upcoming_from(
    streamable_at: Option<i64>,
    release_date_stream: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if let Some(at) = streamable_at {
        return at > now.timestamp();
    }
    release_date_stream
        .and_then(|s| chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
        .map(|date| date > now.date_naive())
        .unwrap_or(false)
}

/// Album summary (embedded in track responses)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub image: ImageSet,
    /// Label (if returned in track response)
    pub label: Option<Label>,
    /// Genre (when returned, e.g. on favorites track album objects).
    pub genre: Option<Genre>,
    /// The album's MAIN artist, when the track's embedded album carries it
    /// (search results and favourites do — captured
    /// `qobuz-api/search-results-response.json`). Distinct from the track's
    /// `performer`, which is who is credited on THAT track; the artist page's
    /// "In library" tab admits a track through either.
    #[serde(default)]
    pub artist: Option<Artist>,
    /// Release/edition suffix on embedded album rows (for example
    /// "Remastered 2021"). Search and favourites payloads carry it even
    /// though older endpoints omit it.
    #[serde(default)]
    pub version: Option<String>,
    /// Availability of the release that owns this track. As with the full
    /// [`Album`] model, absence means unknown — never unavailable.
    #[serde(default)]
    pub streamable: Option<bool>,
    /// Unix timestamp at which this release becomes streamable, when supplied
    /// by the embedding endpoint.
    #[serde(default)]
    pub streamable_at: Option<i64>,
    /// ISO streaming release date, used as the fallback upcoming signal.
    #[serde(default)]
    pub release_date_stream: Option<String>,
    /// Original release date, when included by the embedded album endpoint.
    #[serde(default)]
    pub release_date_original: Option<String>,
}

impl AlbumSummary {
    /// `None` is a terse endpoint, not a withdrawal.
    pub fn is_streamable(&self) -> bool {
        self.streamable.unwrap_or(true)
    }

    /// Distinguish a future release from a withdrawn one using the same rule
    /// as [`Track::is_upcoming`].
    pub fn is_upcoming(&self) -> bool {
        !self.is_streamable()
            && upcoming_from(
                self.streamable_at,
                self.release_date_stream.as_deref(),
                chrono::Utc::now(),
            )
    }
}

/// Album model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    #[serde(default)]
    pub id: String,
    /// When the user favourited it — `favorite/getUserFavorites` items carry
    /// it (unix seconds); every other album shape leaves it `None`.
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub favorited_at: Option<i64>,
    /// Store flag from the catalog (`purchasable`): `Some(false)` is the API
    /// saying this release is not sold on Qobuz; absence is unknown, not no.
    #[serde(default)]
    pub purchasable: Option<bool>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: Artist,
    #[serde(default)]
    pub image: ImageSet,
    pub release_date_original: Option<String>,
    /// Date the album becomes available for streaming (ISO YYYY-MM-DD).
    /// When in the future, the album is upcoming and cannot be fetched
    /// via `get_album` yet — Release Watch uses this to gate clicks.
    pub release_date_stream: Option<String>,
    /// Whether the album is currently streamable. False for upcoming
    /// releases, regional restrictions, or label takedowns.
    #[serde(default)]
    pub streamable: Option<bool>,
    pub label: Option<Label>,
    pub genre: Option<Genre>,
    pub tracks_count: Option<u32>,
    pub duration: Option<u32>,
    #[serde(default)]
    pub hires: bool,
    #[serde(default)]
    pub hires_streamable: bool,
    pub maximum_sampling_rate: Option<f64>,
    pub maximum_bit_depth: Option<u32>,
    /// V2 nested quality block. The modern album shape returned by
    /// `/label/getAlbums` (DiscographyAlbumDto) and `/discover`-style items
    /// nests quality here; preferred over the flat `maximum_*` fields.
    #[serde(default)]
    pub audio_info: Option<DiscoverAudioInfo>,
    /// V2 nested release dates (`{original, download, stream}`); preferred
    /// over the flat `release_date_original` when present.
    #[serde(default)]
    pub dates: Option<DiscoverAlbumDates>,
    /// The V2 wire spells the album track count `track_count` (no trailing
    /// `s`); the flat shape uses `tracks_count`.
    #[serde(default)]
    pub track_count: Option<u32>,
    /// Explicit release type when provided ("album" | "ep" | "single" |
    /// "live" | "compilation" | ...).
    #[serde(default)]
    pub release_type: Option<String>,
    #[serde(default)]
    pub tracks: Option<TracksContainer>,
    /// Universal Product Code for the album
    pub upc: Option<String>,
    /// Editorial description/review of the album
    pub description: Option<String>,
    /// Album goodies (booklets, liner notes PDFs, videos).
    ///
    /// LENIENT on purpose. `/album/get` is parsed with the strict structs — its
    /// `Option<T>` fields carry no lenient wrapper, so ONE wrong-typed value
    /// fails the entire album parse and the album simply will not open. That
    /// risk is concentrated exactly here: on albums nobody owns, `goodies` comes
    /// back an empty array, so its populated item shape has never been observed.
    /// The vendor's own downloader gates goodie fetching behind purchases, which
    /// is the hypothesis for why. We cannot capture a populated one without
    /// owning a purchase, so the shape stays UNVERIFIED — and an unverified
    /// shape must degrade to `None`, never take the album down with it.
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub goodies: Option<Vec<Goody>>,
    /// Editorial awards (Qobuzissime, Album of the Week, press accolades).
    #[serde(default)]
    pub awards: Option<Vec<AlbumAward>>,
    /// Parental advisory / explicit content marker.
    #[serde(default)]
    pub parental_warning: Option<bool>,
    /// Full artist contributor list including roles. The primary artist is
    /// duplicated here as `roles: ["main-artist"]`; non-main entries are
    /// the album's featured artists.
    #[serde(default)]
    pub artists: Option<Vec<AlbumArtist>>,
    /// Release variant label ("2009 Remaster", "Hi-Res", "Deluxe Edition", …).
    /// Qobuz keeps this out of `title`; the web player appends it in parens so
    /// re-editions of the same album are distinguishable. Surfaced the same way
    /// on every album title (see `format_album_title`).
    #[serde(default)]
    pub version: Option<String>,
    /// Album-level composer credit (single Artist). The official web player
    /// renders this — NOT the per-track `composer` — as the "… • X
    /// (composer)" tail of the header credit line, and suppresses it when the
    /// name is the "Various Composers" placeholder. See `album::build_credits`.
    #[serde(default)]
    pub composer: Option<Artist>,
}

impl Album {
    /// Interpret the catalog's optional availability bit without turning an
    /// omitted field into a withdrawal. Only an explicit `false` means the
    /// album is unavailable.
    pub fn is_streamable(&self) -> bool {
        self.streamable.unwrap_or(true)
    }
}

/// Album artist contributor entry (main artist + featured artists).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumArtist {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub roles: Option<Vec<String>>,
}

/// A downloadable extra bundled with an album (e.g. PDF booklet).
///
/// Every field defaults, so a missing key never fails an item, and the whole
/// list is behind `lenient_option` on `Album::goodies`, so a surprising list or
/// item shape degrades to "no goodies" rather than to "the album will not open".
/// Read `url`/`name` through [`Goody::best_url`] / [`Goody::display_name`] rather
/// than directly — the populated shape has never been observed and those helpers
/// are where the fallbacks live.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goody {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    /// Original (full-size) URL
    #[serde(default)]
    pub original_url: String,
    /// A third spelling seen in vendor payloads; kept as a fallback source for
    /// [`Goody::best_url`] because we cannot confirm which key a real,
    /// purchase-gated goodie uses.
    #[serde(default)]
    pub file_url: Option<String>,
    /// File format id (e.g. 21 for PDF)
    #[serde(default)]
    pub file_format_id: Option<u32>,
    #[serde(default)]
    pub description: Option<String>,
}

impl Goody {
    /// The URL to fetch, trying every spelling we know of. `original_url` wins
    /// because it is documented as the full-size asset; `url` is the common
    /// shape; `file_url` is the unconfirmed third. Returns `None` when the item
    /// carries no usable URL at all, which is the signal to skip it rather than
    /// to fail anything.
    pub fn best_url(&self) -> Option<&str> {
        [
            self.original_url.as_str(),
            self.url.as_str(),
            self.file_url.as_deref().unwrap_or(""),
        ]
        .into_iter()
        .map(str::trim)
        .find(|candidate| !candidate.is_empty())
    }

    /// A human label, falling back through the fields most likely to carry one
    /// and finally to the id, so a goodie is never rendered nameless.
    pub fn display_name(&self) -> String {
        for candidate in [Some(self.name.as_str()), self.description.as_deref()]
            .into_iter()
            .flatten()
        {
            let trimmed = candidate.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        format!("Goody {}", self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracksContainer {
    pub items: Vec<Track>,
    pub total: u32,
}

/// Artist model
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Artist {
    #[serde(default)]
    pub id: u64,
    /// Favourites-list rows only (unix seconds); see `Album::favorited_at`.
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub favorited_at: Option<i64>,
    #[serde(default)]
    pub name: String,
    pub image: Option<ImageSet>,
    #[serde(default)]
    pub albums_count: Option<u32>,
    /// Biography (available when fetching full artist details)
    #[serde(default)]
    pub biography: Option<ArtistBiography>,
    /// Albums (available when fetching with extra=albums)
    #[serde(default)]
    pub albums: Option<ArtistAlbums>,
    /// Tracks where this artist appears (extra=tracks_appears_on)
    #[serde(default)]
    pub tracks_appears_on: Option<TracksContainer>,
    /// Curated playlists for this artist (extra=playlists)
    #[serde(default)]
    pub playlists: Option<Vec<Playlist>>,
}

/// Artist biography content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistBiography {
    pub summary: Option<String>,
    pub content: Option<String>,
    pub source: Option<String>,
}

/// Artist albums container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistAlbums {
    pub items: Vec<Album>,
    pub total: u32,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub limit: u32,
}

/// Playlist model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub owner: PlaylistOwner,
    pub images: Option<Vec<String>>,
    #[serde(default)]
    pub tracks_count: u32,
    #[serde(default)]
    pub duration: u32,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub tracks: Option<TracksContainer>,
    pub genres: Option<Vec<PlaylistGenre>>,
    pub images150: Option<Vec<String>>,
    pub images300: Option<Vec<String>>,
    /// The playlist's OWN Qobuz artwork (editorial playlists). The `images*`
    /// lists above are MEMBER-ALBUM covers, which is why a Qobuz playlist
    /// card binding them shows an album sleeve instead of the playlist
    /// graphic. Absent on user playlists, which fall back to the collage.
    #[serde(default)]
    pub image_rectangle: Option<Vec<String>>,
    #[serde(default)]
    pub image_rectangle_mini: Option<Vec<String>>,
    pub slug: Option<String>,
    pub users_count: Option<u32>,
    /// API revision stamp (unix seconds). The membership index reads it as
    /// change evidence when the API provides it; absent on some list shapes.
    #[serde(default)]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaylistOwner {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistGenre {
    pub id: u64,
    pub name: String,
    pub slug: Option<String>,
}

/// Lightweight playlist response with track IDs only
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistWithTrackIds {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub owner: PlaylistOwner,
    pub images: Option<Vec<String>>,
    #[serde(default)]
    pub tracks_count: u32,
    #[serde(default)]
    pub duration: u32,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub track_ids: Vec<u64>,
    pub genres: Option<Vec<PlaylistGenre>>,
    pub images150: Option<Vec<String>>,
    pub images300: Option<Vec<String>>,
    pub slug: Option<String>,
    pub users_count: Option<u32>,
    /// API revision stamp (unix seconds), when the response carries one.
    #[serde(default)]
    pub updated_at: Option<i64>,
}

/// Result of checking for duplicate tracks in a playlist
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistDuplicateResult {
    pub total_tracks: usize,
    pub duplicate_count: usize,
    pub duplicate_track_ids: HashSet<u64>,
}

// ============ Metadata Types ============

/// Label model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: u64,
    pub name: String,
}

// ============ Label Page Types (/label/page) ============

/// Top-level response from /label/page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelPageData {
    pub id: u64,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub image: Option<serde_json::Value>,
    #[serde(default)]
    pub releases: Option<Vec<LabelPageContainer>>,
    #[serde(default)]
    pub playlists: Option<LabelPageGenericList>,
    #[serde(default)]
    pub top_tracks: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub top_artists: Option<LabelPageGenericList>,
}

/// A container within label page (e.g. releases category)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelPageContainer {
    pub id: Option<String>,
    pub data: Option<LabelPageGenericList>,
}

/// Generic list with has_more and items
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelPageGenericList {
    pub has_more: Option<bool>,
    pub items: Option<Vec<serde_json::Value>>,
}

// ============ Award Page Types (/award/page) ============

/// Magazine/publisher behind a press award.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwardMagazine {
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
}

/// Top-level response from /award/page. Fields all Optional because
/// Android's AwardDto marks everything nullable and Qobuz is loose
/// about which ones come back on any given request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwardPageData {
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    pub awarded_at: Option<String>,
    #[serde(default)]
    pub magazine: Option<AwardMagazine>,
    /// Categorized containers of award-winning releases (matches
    /// Android's `releases: List<GenericContainerDto<AlbumDto>>`).
    #[serde(default)]
    pub releases: Option<Vec<AwardPageContainer>>,
    #[serde(default)]
    pub playlists: Option<AwardPageGenericList>,
}

fn deserialize_string_or_int<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwardPageContainer {
    pub id: Option<String>,
    pub data: Option<AwardPageGenericList>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwardPageGenericList {
    pub has_more: Option<bool>,
    pub items: Option<Vec<serde_json::Value>>,
}

/// Response from /label/explore
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelExploreResponse {
    pub has_more: Option<bool>,
    pub items: Option<Vec<serde_json::Value>>,
}

// ============ Label Sub-resource Types (v9.7.0.3 API) ============
//
// The label page (/label/page) returns an aggregated snapshot; the
// getAlbums / getPlaylists / getTopArtists / getNextReleases /
// getAwardedReleases endpoints return paginated lists for each
// sub-resource. Per Qobuz convention these use the V2 list envelope
// { has_more, items: [...] }. Deserialized shapes are best-effort: if
// the server wraps items in e.g. { albums: { items: ... } }, the
// Optional fallbacks still keep the call non-fatal.

/// Generic paginated response from /label/get* endpoints.
///
/// `T` is typed (`Album`, `Playlist`, `Artist`) but all fields are
/// tolerant so the same struct works if the server returns a bare
/// items list or a nested one.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LabelListPage<T> {
    #[serde(default)]
    pub has_more: Option<bool>,
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Response from /label/story.
///
/// Shape inferred from `c30/b.java` — returns editorial / story content
/// for a label. Actual fields beyond the label identity are not fully
/// known; everything past `id` / `name` / `description` is kept open.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelStoryResponse {
    pub id: Option<u64>,
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub story: Option<String>,
    #[serde(default)]
    pub image: Option<serde_json::Value>,
    #[serde(default)]
    pub has_more: Option<bool>,
    #[serde(default)]
    pub items: Option<Vec<serde_json::Value>>,
}

/// Response from /label/getList (POST). Bulk lookup that hydrates
/// label metadata for a set of label IDs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LabelGetListResponse {
    #[serde(default = "Vec::new")]
    pub labels: Vec<Label>,
    /// Fallback for unknown envelope shape — preserved as raw JSON if
    /// the server wraps differently than expected.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

/// Genre model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Genre {
    pub id: u64,
    pub name: String,
    /// Full ancestor id chain (top-level first, self last) as sent by the
    /// discover endpoints. Absent on older cached payloads → None.
    #[serde(default)]
    pub path: Option<Vec<u64>>,
}

/// Genre info with full details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenreInfo {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub path: Option<Vec<u64>>,
}

/// Genre list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenreListResponse {
    pub genres: GenreListContainer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenreListContainer {
    pub items: Vec<GenreInfo>,
}

// ============ Search Types ============

/// Search results container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    pub albums: Option<SearchResultsPage<Album>>,
    pub tracks: Option<SearchResultsPage<Track>>,
    pub artists: Option<SearchResultsPage<Artist>>,
    pub playlists: Option<SearchResultsPage<Playlist>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchResultsPage<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    // `/album/suggest` returns a page with only `{limit, items}` (no `total`
    // or `offset`); without defaults the whole response failed to deserialize
    // and the album "Suggestions" carousel silently never showed. Defaulting
    // the pagination scalars to 0 is harmless — only `items` is consumed there.
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub limit: u32,
}

// ============ Purchases API Models ============
//
// Ported field-for-field from `src-tauri/src/api/models.rs:546-628`. These are
// the wire shapes returned by the Qobuz `/purchase/*` endpoints. The lenient
// deserializers live in `crate::purchase_serde` (see that module's docs for the
// per-field coercion rules).

/// Response from `/purchase/getUserPurchases` (commands #1/#3/#4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseResponse {
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_page")]
    pub albums: SearchResultsPage<PurchaseAlbum>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_page")]
    pub tracks: SearchResultsPage<PurchaseTrack>,
}

/// Response from `/purchase/getUserPurchasesIds` (command #2). Items are OPAQUE
/// JSON — the UI reads only `.total` from each page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseIdsResponse {
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_page")]
    pub albums: SearchResultsPage<serde_json::Value>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_page")]
    pub tracks: SearchResultsPage<serde_json::Value>,
}

/// A purchased album. `downloadable` defaults TRUE; `downloaded` is NOT from
/// Qobuz — it is server-computed from the local registry. `purchased_at` is
/// unix epoch seconds. Nested `tracks` is populated only on the album-detail /
/// by-type-albums paths.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PurchaseAlbum {
    #[serde(
        default,
        deserialize_with = "crate::purchase_serde::deserialize_string_id"
    )]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: Artist,
    #[serde(default)]
    pub image: ImageSet,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub release_date_original: Option<String>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub label: Option<Label>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub genre: Option<Genre>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub tracks_count: Option<u32>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub duration: Option<u32>,
    #[serde(default)]
    pub hires: bool,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub maximum_sampling_rate: Option<f64>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub maximum_bit_depth: Option<u32>,
    #[serde(default = "crate::purchase_serde::serde_true")]
    pub downloadable: bool,
    /// THE ENTITLEMENT: the `format_id`s this purchase may be downloaded in.
    /// First seen on a real account 2026-09-01 — `[55]` (DSD64), `[56]`
    /// (DSD128), `[7, 6, 5]` (a hi-res FLAC purchase) — and it is the only
    /// authority for the download menu: a DSD purchase reports `hires:false`
    /// with a 16/44.1 streaming catalog, so nothing can be inferred from the
    /// album's own quality fields. Empty on older captures.
    #[serde(default)]
    pub downloadable_format_ids: Vec<u32>,
    /// Purchased AS hi-res, independent of what the catalog streams
    /// (`hires:false, hires_purchased:true` on the DSD64 album).
    #[serde(default)]
    pub hires_purchased: bool,
    #[serde(default)]
    pub downloaded: bool,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub purchased_at: Option<i64>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub tracks: Option<SearchResultsPage<PurchaseTrack>>,
}

/// A purchased track.
///
/// It DOES carry a `version` (the track subtitle/edition). It deliberately did
/// not, on the reasoning that the purchases endpoints never send one — which is
/// true of the endpoints and beside the point for the screen: the album-detail
/// view builds its tracks from the CATALOG album (`/album/get`), which does send
/// `version`, and the reference's frontend renders
/// `formatTrackTitle(title, version)`. Dropping the field is what made every
/// purchased track lose its subtitle (issue #360); contract §10-C rules the
/// field back in and `build_purchase_album` maps it across.
///
/// `streamable` defaults TRUE; `downloaded`/`downloaded_format_ids` are
/// server-computed from the local registry; `media_number` is the disc number
/// used for disc-grouping. `purchased_at` is unix epoch seconds.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PurchaseTrack {
    #[serde(
        default,
        deserialize_with = "crate::purchase_serde::deserialize_u64_id"
    )]
    pub id: u64,
    #[serde(default)]
    pub title: String,
    /// Track subtitle ("Remastered", "Live at …"). The purchases endpoints do
    /// not send it, but the catalog `/album/get` track does, and the detail
    /// screen renders `formatTrackTitle(title, version)` — so `build_purchase_album`
    /// carries it across. Tauri declared the same field on its frontend type and
    /// never mapped it, which is why purchased track titles lost their version
    /// (issue #360); this field is what closes that.
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub version: Option<String>,
    #[serde(default)]
    pub track_number: u32,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub media_number: Option<u32>,
    #[serde(default)]
    pub duration: u32,
    #[serde(default)]
    pub performer: Artist,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub album: Option<AlbumSummary>,
    #[serde(default)]
    pub hires: bool,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub maximum_sampling_rate: Option<f64>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub maximum_bit_depth: Option<u32>,
    #[serde(default = "crate::purchase_serde::serde_true")]
    pub streamable: bool,
    #[serde(default)]
    pub downloaded: bool,
    #[serde(default)]
    pub downloaded_format_ids: Vec<u32>,
    /// The per-track entitlement, same meaning as the album's. Not yet observed
    /// on a real `type=tracks` page (every capture so far had zero standalone
    /// track purchases); defaulted so an absent key costs nothing.
    #[serde(default)]
    pub downloadable_format_ids: Vec<u32>,
    #[serde(default, deserialize_with = "crate::purchase_serde::lenient_option")]
    pub purchased_at: Option<i64>,
}

/// A downloadable format option synthesized client-side from an `Album`
/// (command #6). `id` feeds `getFileUrl`'s `format_id`; `label` (with `/`→`-`)
/// becomes the `qualityDir` subfolder. The synthesis table lives in the
/// orchestration service (Slice 4); this struct is just the wire/UI shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseFormatOption {
    pub id: u32,
    pub label: String,
    pub bit_depth: Option<u32>,
    pub sampling_rate: Option<f64>,
    /// `false` = the purchased file, fetched through the purchase grant
    /// (`getFileUrl`, `intent=download`), which the store only issues for the
    /// ids in the entitlement. `true` = a lower quality the store does NOT
    /// sell for this purchase, fetched through the streaming grant
    /// (`intent=stream`) instead — the route the Slint/Tauri builds used for
    /// every download, kept only BELOW the purchased quality.
    #[serde(default)]
    pub streaming: bool,
}

/// Response from `/album/suggest` — albums similar to a seed album.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumSuggestResponse {
    #[serde(default)]
    pub algorithm: Option<String>,
    #[serde(default)]
    pub albums: Option<SearchResultsPage<Album>>,
}

/// Response from the `/radio/*` endpoints — a generated track list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadioResponse {
    #[serde(rename = "type", default)]
    pub radio_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::purchase_serde::lenient_page_flexible"
    )]
    pub tracks: SearchResultsPage<Track>,
}

/// One entry of the Qobuz `most_popular` block in a combined search.
/// Serde tagging matches the legacy `V2MostPopularItem` so the Tauri
/// command's response shape is unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "lowercase")]
pub enum MostPopularItem {
    Tracks(Track),
    Albums(Album),
    Artists(Artist),
}

/// Combined search result: the four category pages plus an optional
/// "most popular" hero entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAllResults {
    pub albums: SearchResultsPage<Album>,
    pub tracks: SearchResultsPage<Track>,
    pub artists: SearchResultsPage<Artist>,
    pub playlists: SearchResultsPage<Playlist>,
    pub most_popular: Option<MostPopularItem>,
}

/// Favorites container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Favorites {
    pub albums: Option<SearchResultsPage<Album>>,
    pub tracks: Option<SearchResultsPage<Track>>,
    pub artists: Option<SearchResultsPage<Artist>>,
}

// ============ Discover API Types ============

/// Discover index response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverResponse {
    pub containers: DiscoverContainers,
}

/// All discover containers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverContainers {
    pub playlists: Option<DiscoverContainer<DiscoverPlaylist>>,
    pub ideal_discography: Option<DiscoverContainer<DiscoverAlbum>>,
    pub playlists_tags: Option<DiscoverContainer<PlaylistTag>>,
    pub new_releases: Option<DiscoverContainer<DiscoverAlbum>>,
    pub qobuzissims: Option<DiscoverContainer<DiscoverAlbum>>,
    pub most_streamed: Option<DiscoverContainer<DiscoverAlbum>>,
    pub press_awards: Option<DiscoverContainer<DiscoverAlbum>>,
    pub album_of_the_week: Option<DiscoverContainer<DiscoverAlbum>>,
}

/// Generic discover container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverContainer<T> {
    pub id: String,
    pub data: DiscoverData<T>,
}

/// Generic discover data with items
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverData<T> {
    pub has_more: bool,
    pub items: Vec<T>,
}

/// Playlist from discover endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverPlaylist {
    pub id: u64,
    pub name: String,
    pub owner: PlaylistOwner,
    pub image: DiscoverPlaylistImage,
    pub description: Option<String>,
    pub duration: u32,
    pub tracks_count: u32,
    pub genres: Option<Vec<PlaylistGenre>>,
    pub tags: Option<Vec<PlaylistTag>>,
}

/// Playlist image from discover
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverPlaylistImage {
    pub rectangle: Option<String>,
    pub covers: Option<Vec<String>>,
}

/// Playlist tag (for filtering)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistTag {
    pub id: u64,
    pub slug: String,
    pub name: String,
}

/// Raw playlist tag from /playlist/getTags
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPlaylistTag {
    pub slug: String,
    pub name_json: String,
    pub position: Option<String>,
    pub is_discover: Option<String>,
    pub featured_tag_id: Option<String>,
}

/// Response from /playlist/getTags
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistTagsResponse {
    pub tags: Vec<RawPlaylistTag>,
}

/// Response from discover/playlists endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverPlaylistsResponse {
    pub has_more: bool,
    pub items: Vec<DiscoverPlaylist>,
}

/// Album from discover endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverAlbum {
    pub id: String,
    pub title: String,
    pub version: Option<String>,
    pub track_count: Option<u32>,
    pub duration: Option<u32>,
    pub parental_warning: Option<bool>,
    pub image: DiscoverAlbumImage,
    pub artists: Vec<DiscoverArtist>,
    pub label: Option<Label>,
    pub genre: Option<Genre>,
    pub dates: Option<DiscoverAlbumDates>,
    pub audio_info: Option<DiscoverAudioInfo>,
    /// Editorial awards attached to the album. Id 88 = Qobuzissime,
    /// id 151 = Qobuz Album of the Week (locale-stable).
    #[serde(default)]
    pub awards: Option<Vec<AlbumAward>>,
}

/// Album image from discover endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverAlbumImage {
    pub small: Option<String>,
    pub thumbnail: Option<String>,
    pub large: Option<String>,
}

/// Artist in discover album
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverArtist {
    pub id: u64,
    pub name: String,
    pub roles: Option<Vec<String>>,
}

/// Album dates from discover
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverAlbumDates {
    pub download: Option<String>,
    pub original: Option<String>,
    pub stream: Option<String>,
}

/// Audio info from discover album (and, since 2026-08-28, from tracks)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverAudioInfo {
    pub maximum_sampling_rate: Option<f64>,
    pub maximum_bit_depth: Option<u32>,
    pub maximum_channel_count: Option<u32>,
    /// ReplayGain track gain (dB) and peak, as Qobuz ships them on every
    /// track in search/album envelopes
    /// (`qbz-nix-docs/qobuz-api/search-results-response.json`, 15 samples).
    /// Modelled only — no reader yet; the loudness analyser stays as is.
    #[serde(default)]
    pub replaygain_track_gain: Option<f64>,
    #[serde(default)]
    pub replaygain_track_peak: Option<f64>,
}

// ============ Artist Page Types (/artist/page) ============

/// Top-level response from /artist/page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistResponse {
    pub id: u64,
    pub name: PageArtistName,
    pub artist_category: Option<String>,
    pub biography: Option<PageArtistBiography>,
    pub images: Option<PageArtistImages>,
    pub similar_artists: Option<PageArtistSimilar>,
    pub top_tracks: Option<Vec<PageArtistTrack>>,
    pub last_release: Option<PageArtistRelease>,
    pub releases: Option<Vec<PageArtistReleaseGroup>>,
    pub tracks_appears_on: Option<Vec<PageArtistTrack>>,
    pub playlists: Option<PageArtistPlaylists>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistName {
    pub display: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistBiography {
    pub content: Option<String>,
    pub source: Option<serde_json::Value>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistImages {
    pub portrait: Option<PageArtistPortrait>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistPortrait {
    pub hash: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistSimilar {
    pub has_more: bool,
    pub items: Vec<PageArtistSimilarItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistSimilarItem {
    pub id: u64,
    pub name: PageArtistName,
    pub images: Option<PageArtistImages>,
}

/// A group of releases by type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistReleaseGroup {
    #[serde(rename = "type")]
    pub release_type: String,
    pub has_more: bool,
    pub items: Vec<PageArtistRelease>,
}

/// A release item from /artist/page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistRelease {
    pub id: String,
    pub title: String,
    pub version: Option<String>,
    pub tracks_count: Option<u32>,
    pub artist: Option<PageArtistReleaseArtist>,
    pub artists: Option<Vec<PageArtistReleaseContributor>>,
    pub image: Option<ImageSet>,
    pub label: Option<Label>,
    pub genre: Option<Genre>,
    pub release_type: Option<String>,
    pub release_tags: Option<Vec<String>>,
    pub duration: Option<u32>,
    pub dates: Option<DiscoverAlbumDates>,
    pub parental_warning: Option<bool>,
    pub audio_info: Option<DiscoverAudioInfo>,
    pub rights: Option<PageArtistRights>,
    pub awards: Option<Vec<PageArtistAward>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistReleaseArtist {
    pub id: u64,
    pub name: PageArtistName,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistReleaseContributor {
    pub id: u64,
    pub name: String,
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistRights {
    pub streamable: Option<bool>,
    pub hires_streamable: Option<bool>,
    pub hires_purchasable: Option<bool>,
    pub purchasable: Option<bool>,
    pub downloadable: Option<bool>,
    pub previewable: Option<bool>,
    pub sampleable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistAward {
    pub id: u64,
    pub name: String,
    pub awarded_at: Option<String>,
}

/// Award attached to an album. Shape is intentionally lenient because
/// Qobuz uses three different embedded shapes across endpoints:
/// - `/discover/index` — {id: int, name, awarded_at: "YYYY-MM-DD"}
/// - `/album/get`      — LegacyAwardDto {awardId: string, name,
///                        publicationId, publicationName, awardSlug,
///                        awardedAt: long, …}
/// - `/artist/page`    — PageArtistAward {id: int, name, awarded_at}
/// id is emitted as String downstream so the frontend has a single
/// type to carry into /award/page and /award/getAlbums. The `alias`
/// list covers the LegacyAwardDto field name the web app never sees
/// but the mobile API uses.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AlbumAward {
    #[serde(
        default,
        alias = "awardId",
        alias = "award_id",
        deserialize_with = "deserialize_award_id"
    )]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(
        default,
        alias = "awardedAt",
        deserialize_with = "deserialize_award_awarded_at"
    )]
    pub awarded_at: Option<String>,
}

fn deserialize_award_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

fn deserialize_award_awarded_at<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

/// Track from /artist/page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistTrack {
    pub id: u64,
    pub title: String,
    pub version: Option<String>,
    pub duration: Option<u32>,
    pub isrc: Option<String>,
    pub parental_warning: Option<bool>,
    pub artist: Option<PageArtistReleaseArtist>,
    pub composer: Option<serde_json::Value>,
    pub audio_info: Option<DiscoverAudioInfo>,
    pub rights: Option<PageArtistRights>,
    pub physical_support: Option<PageArtistPhysicalSupport>,
    pub album: Option<PageArtistTrackAlbum>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistPhysicalSupport {
    pub media_number: Option<u32>,
    pub track_number: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistTrackAlbum {
    pub id: String,
    pub title: String,
    pub version: Option<String>,
    pub image: Option<ImageSet>,
    pub label: Option<Label>,
    pub genre: Option<Genre>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistPlaylists {
    pub has_more: bool,
    pub items: Vec<PageArtistPlaylist>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistPlaylist {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub owner: Option<PageArtistPlaylistOwner>,
    pub tracks_count: Option<u32>,
    pub duration: Option<u32>,
    pub images: Option<PageArtistPlaylistImages>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistPlaylistOwner {
    pub id: u64,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageArtistPlaylistImages {
    pub rectangle: Option<Vec<String>>,
}

/// Response from /artist/getReleasesGrid
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasesGridResponse {
    pub has_more: bool,
    pub items: Vec<PageArtistRelease>,
}

// ============ Artist Story Types (/artist/story) ============

/// Response from /artist/story (Magazine / editorial articles about the artist).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistStoryResponse {
    pub has_more: bool,
    #[serde(default)]
    pub items: Vec<ArtistStoryItem>,
}

/// A single Magazine story. `image`/`images[].url` are ready-to-use signed
/// arc-cdn URLs — do NOT run them through the portrait hash/format builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistStoryItem {
    pub id: String,
    pub title: String,
    /// Epoch SECONDS, not an ISO string.
    #[serde(default)]
    pub display_date: Option<i64>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub images: Option<Vec<ArtistStoryImage>>,
    #[serde(default)]
    pub description_short: Option<String>,
    #[serde(default)]
    pub authors: Option<Vec<ArtistStoryAuthor>>,
    #[serde(default)]
    pub section_slugs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistStoryImage {
    #[serde(default)]
    pub format: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistStoryAuthor {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_session_deserializes_pre_v10_json() {
        // Sessions persisted before the country/language capture must still
        // load: both new fields default to None (feature stays Auto-off).
        let json = r#"{
            "user_auth_token": "token",
            "user_id": 1705826,
            "email": "a@b.c",
            "display_name": "Tester",
            "subscription_label": "Studio",
            "subscription_valid_until": null
        }"#;
        let session: UserSession = serde_json::from_str(json).expect("old session json loads");
        assert_eq!(session.country_code, None);
        assert_eq!(session.language_code, None);
        assert_eq!(session.user_id, 1705826);
    }

    #[test]
    fn user_session_round_trips_country_and_language() {
        let json = r#"{
            "user_auth_token": "token",
            "user_id": 1705826,
            "email": "a@b.c",
            "display_name": "Tester",
            "subscription_label": "Studio",
            "subscription_valid_until": null,
            "country_code": "FR",
            "language_code": "fr"
        }"#;
        let session: UserSession = serde_json::from_str(json).expect("v10 session json loads");
        assert_eq!(session.country_code.as_deref(), Some("FR"));
        assert_eq!(session.language_code.as_deref(), Some("fr"));
        let back: UserSession =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(back.language_code.as_deref(), Some("fr"));
    }

    fn image_set() -> ImageSet {
        ImageSet {
            small: Some("s50".into()),
            thumbnail: Some("t150".into()),
            large: Some("l300".into()),
            extralarge: Some("x600".into()),
            mega: Some("m3000".into()),
            back: None,
        }
    }

    #[test]
    fn for_px_picks_the_smallest_covering_variant() {
        let set = image_set();
        assert_eq!(set.for_px(72).map(String::as_str), Some("t150"));
        assert_eq!(set.for_px(150).map(String::as_str), Some("t150"));
        assert_eq!(set.for_px(151).map(String::as_str), Some("l300"));
        assert_eq!(set.for_px(300).map(String::as_str), Some("l300"));
        assert_eq!(set.for_px(660).map(String::as_str), Some("m3000"));
        // Past mega there is nothing bigger: best() is the ceiling.
        assert_eq!(set.for_px(4000).map(String::as_str), Some("m3000"));
    }

    #[test]
    fn for_px_skips_missing_variants_upward_on_partial_sets() {
        // Discover endpoints return only small/thumbnail/large.
        let discover = ImageSet {
            small: Some("s50".into()),
            thumbnail: Some("t150".into()),
            large: Some("l300".into()),
            ..ImageSet::default()
        };
        assert_eq!(discover.for_px(72).map(String::as_str), Some("t150"));
        // Nothing >= 600: best-available (large), never an upscale promise.
        assert_eq!(discover.for_px(600).map(String::as_str), Some("l300"));
        // A gap below the request keeps scanning UP: {small, mega} at 200px
        // is served by mega — the smallest variant that COVERS the slot (the
        // "never upscale into a slot" rule wins over download size; full
        // album/track sets populate all five variants, so this only bites on
        // genuinely sparse sets).
        let gapped = ImageSet {
            small: Some("s50".into()),
            mega: Some("m3000".into()),
            ..ImageSet::default()
        };
        assert_eq!(gapped.for_px(200).map(String::as_str), Some("m3000"));
        assert_eq!(gapped.for_px(72).map(String::as_str), Some("m3000"));
    }

    #[test]
    fn for_px_never_serves_worse_than_best() {
        let tiny = ImageSet {
            small: Some("s50".into()),
            ..ImageSet::default()
        };
        assert_eq!(tiny.for_px(1600).map(String::as_str), Some("s50"));
        assert_eq!(ImageSet::default().for_px(300), None);
    }

    #[test]
    fn bucket_for_px_maps_slots_onto_the_variant_table() {
        assert_eq!(ImageSet::bucket_for_px(0), IMAGE_VARIANT_PX[0]);
        assert_eq!(ImageSet::bucket_for_px(72), 150);
        assert_eq!(ImageSet::bucket_for_px(150), 150);
        assert_eq!(ImageSet::bucket_for_px(151), 300);
        assert_eq!(ImageSet::bucket_for_px(660), 3000);
        // Beyond the table the bucket saturates at mega, so a 4K window does
        // not invent a size no variant serves.
        assert_eq!(ImageSet::bucket_for_px(5000), 3000);
    }

    #[test]
    fn qobuz_cover_at_px_rewrites_the_size_suffix() {
        let small = "https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_50.jpg";
        assert_eq!(
            qobuz_cover_at_px(small, 600).as_deref(),
            Some("https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_600.jpg")
        );
        // Slots bucket UP to the smallest covering variant.
        assert_eq!(
            qobuz_cover_at_px(small, 250).as_deref(),
            Some("https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_300.jpg")
        );
        assert_eq!(
            qobuz_cover_at_px(small, 5000).as_deref(),
            Some("https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_max.jpg")
        );
        // The mega bucket emits the literal `_max` suffix, never `_3000`
        // (a 404 on the live CDN, measured 2026-08-15).
        assert_eq!(
            qobuz_cover_at_px(small, 700).as_deref(),
            Some("https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_max.jpg")
        );
        // Down-tiering works too (a mega `_max` carried into a 150px slot).
        let mega = "https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_max.jpg";
        assert_eq!(
            qobuz_cover_at_px(mega, 100).as_deref(),
            Some("https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_150.jpg")
        );
    }

    #[test]
    fn qobuz_cover_at_px_leaves_unrecognized_urls_alone() {
        // Not a cover url, no size suffix, an unknown size, a query string —
        // all None, so callers keep the original.
        assert_eq!(qobuz_cover_at_px("https://example.com/a_50.jpg", 600), None);
        assert_eq!(
            qobuz_cover_at_px(
                "https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb.jpg",
                600
            ),
            None
        );
        assert_eq!(
            qobuz_cover_at_px(
                "https://static.qobuz.com/images/covers/pb/ap/gxy13gb56appb_123.jpg",
                600
            ),
            None
        );
        assert_eq!(
            qobuz_cover_at_px(
                "https://static.qobuz.com/images/covers/pb/ap/x_50.jpg?token=1",
                600
            ),
            None
        );
        assert_eq!(qobuz_cover_at_px("/home/u/Music/cover.jpg", 600), None);
    }
}

// ─── §12-4: purchase deserializers, one payload per missing optional field ────
//
// The whole point of this module is that the purchase path cannot be smoke-
// tested: Qobuz Purchases is not sold in the owner's region, so the only
// populated payloads anyone will ever see belong to end users. Every default
// below is therefore asserted rather than assumed, and the ones that DIFFER
// between two structs with the same field name get their own test, because that
// is the trap most likely to invert a screen's behaviour silently.
#[cfg(test)]
mod purchase_deserializer_tests {
    use super::*;

    // ── §2.6: the `streamable` split ─────────────────────────────────────────
    //
    // Two structs, same field name, three semantics. `PurchaseTrack.streamable`
    // is a `bool` defaulting TRUE (`serde_true`) — the purchases endpoints are
    // terse and that screen gates click-to-play on the flag, so a default of
    // false would make every purchased album unclickable. Catalog
    // `Track.streamable` is now `Option<bool>`: it does not guess in EITHER
    // direction, because the greyout and the queue filter key on it and a wrong
    // guess is visible on every screen in the app. Absence is resolved in
    // exactly one function, `Track::is_streamable`, and nowhere else.

    /// A purchases-list track defaults `streamable` to **true**.
    #[test]
    fn purchase_track_streamable_defaults_true() {
        let t: PurchaseTrack = serde_json::from_str(r#"{"id":1,"title":"T"}"#).unwrap();
        assert!(
            t.streamable,
            "PurchaseTrack.streamable defaults TRUE (serde_true)"
        );
    }

    /// A CATALOG track that does not mention `streamable` deserializes to
    /// `None`, and `None` reads as **available**.
    ///
    /// This is the regression that would otherwise grey out the whole
    /// catalogue. While the field was a `bool` with a bare `#[serde(default)]`,
    /// an endpoint that omits the key produced `false` — "pulled from Qobuz" —
    /// for every track it returned. `/album/get` does send the key (captured
    /// 2026-08-17), but `/playlist/get?extra=tracks` never has been, and that is
    /// the surface where dead tracks actually show up.
    #[test]
    fn catalog_track_streamable_absent_is_none_and_reads_available() {
        let t: Track = serde_json::from_str(r#"{"id":1,"title":"T"}"#).unwrap();
        assert_eq!(
            t.streamable, None,
            "an absent key must stay absent — never collapse into Some(false)"
        );
        assert!(
            t.is_streamable(),
            "absence is NOT a claim of unavailability: absent reads as available"
        );
    }

    /// An explicit `"streamable": false` is honoured, and it is the ONLY shape
    /// that marks a track as pulled.
    ///
    /// This is the real wire signal, captured from
    /// `/album/get?album_id=0886443985094` on 2026-08-17: track 1 carries
    /// `false` (and keeps its `isrc`, which is what makes the replacement
    /// search's ISRC short-circuit reachable) while the album around it is
    /// `true`.
    #[test]
    fn catalog_track_streamable_false_is_honoured() {
        let t: Track = serde_json::from_str(r#"{"id":1,"title":"T","streamable":false}"#).unwrap();
        assert_eq!(t.streamable, Some(false));
        assert!(
            !t.is_streamable(),
            "Some(false) is the ONLY shape that marks a track unavailable"
        );
    }

    #[test]
    fn catalog_album_streamable_absent_reads_available() {
        let album: Album = serde_json::from_str(r#"{"id":"a","title":"A"}"#).unwrap();
        assert_eq!(album.streamable, None);
        assert!(album.is_streamable());
    }

    #[test]
    fn catalog_album_streamable_false_is_honoured() {
        let album: Album =
            serde_json::from_str(r#"{"id":"a","title":"A","streamable":false}"#).unwrap();
        assert_eq!(album.streamable, Some(false));
        assert!(!album.is_streamable());
    }

    #[test]
    fn embedded_album_availability_keeps_unknown_distinct_from_false() {
        let absent: AlbumSummary = serde_json::from_str(r#"{"id":"a","title":"A"}"#).unwrap();
        assert_eq!(absent.streamable, None);
        assert!(absent.is_streamable());

        let withdrawn: AlbumSummary = serde_json::from_str(
            r#"{"id":"a","title":"A","version":"Remastered","streamable":false}"#,
        )
        .unwrap();
        assert_eq!(withdrawn.streamable, Some(false));
        assert!(!withdrawn.is_streamable());
        assert!(!withdrawn.is_upcoming());
        assert_eq!(withdrawn.version.as_deref(), Some("Remastered"));
    }

    #[test]
    fn embedded_future_album_is_upcoming_not_withdrawn() {
        let upcoming: AlbumSummary = serde_json::from_str(
            r#"{"id":"a","title":"A","streamable":false,"release_date_stream":"2999-01-01"}"#,
        )
        .unwrap();
        assert!(upcoming.is_upcoming());
    }

    /// `downloadable` drives three list behaviours (the hide-unavailable filter,
    /// the album click gate, the unavailable marker), so its default is not
    /// decoration: getting it wrong ships clickable unavailable albums.
    #[test]
    fn purchase_album_downloadable_defaults_true() {
        let a: PurchaseAlbum = serde_json::from_str(r#"{"id":"x","title":"A"}"#).unwrap();
        assert!(a.downloadable);
        assert!(
            !a.downloaded,
            "downloaded is local-only, never from the wire"
        );
        assert!(a.tracks.is_none());
        assert!(a.purchased_at.is_none());
    }

    // ── §2.5: leniency is load-bearing, and has a documented limit ────────────

    /// Wrong-typed optional fields degrade to `None` instead of failing the item.
    #[test]
    fn wrong_typed_optionals_degrade_to_none() {
        let json = r#"{
            "id": 7, "title": "T",
            "media_number": "not a number",
            "maximum_sampling_rate": {"nope": true},
            "maximum_bit_depth": [],
            "album": 12345,
            "purchased_at": "yesterday",
            "version": []
        }"#;
        let t: PurchaseTrack = serde_json::from_str(json).unwrap();
        assert_eq!(t.id, 7);
        assert_eq!(t.media_number, None);
        assert_eq!(t.maximum_sampling_rate, None);
        assert_eq!(t.maximum_bit_depth, None);
        assert!(t.album.is_none());
        assert_eq!(t.purchased_at, None);
        assert_eq!(t.version, None);
    }

    /// A malformed page collapses to an EMPTY page while its sibling still
    /// parses. This is why a 401/403 body shaped like JSON and "you own nothing"
    /// are indistinguishable — stated here so nobody later mistakes the empty
    /// state for proof that the request succeeded.
    #[test]
    fn a_malformed_page_empties_itself_without_taking_its_sibling() {
        let json = r#"{
            "albums": "this is not a page",
            "tracks": {"offset":0,"limit":50,"total":1,"items":[{"id":9,"title":"Nine"}]}
        }"#;
        let r: PurchaseResponse = serde_json::from_str(json).unwrap();
        assert!(r.albums.items.is_empty());
        assert_eq!(r.albums.total, 0);
        assert_eq!(r.tracks.items.len(), 1);
        assert_eq!(r.tracks.items[0].title, "Nine");
    }

    /// §2.5b-2, measured against a live account: `?type=albums` OMITS the
    /// `tracks` key entirely — it is not present-and-zero. A port that models a
    /// present-but-empty sibling page is modelling the wrong thing.
    #[test]
    fn typed_response_with_an_absent_sibling_page_parses() {
        let json = r#"{
            "albums": {"offset":0,"limit":500,"total":0,"items":[]},
            "user": {"id": 1, "login": "someone"}
        }"#;
        let r: PurchaseResponse = serde_json::from_str(json).unwrap();
        assert!(r.albums.items.is_empty());
        assert!(
            r.tracks.items.is_empty(),
            "absent sibling defaults, never fails"
        );
        assert_eq!(r.tracks.total, 0);
    }

    /// §2.5b-3: the IDS envelope has a DIFFERENT page shape — `{total, items}`
    /// with no `offset` and no `limit`. If those scalars were required, the
    /// lenient page wrapper would swallow the failure and hand back an empty
    /// page, and both tab counters would read 0 forever with nothing logged.
    #[test]
    fn ids_response_page_shape_preserves_total_without_offset_or_limit() {
        let json = r#"{
            "albums": {"total": 42, "items": [1,2,3]},
            "tracks": {"total": 7, "items": []},
            "user": {"id": 1, "login": "someone"}
        }"#;
        let r: PurchaseIdsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.albums.total, 42, "the tab counter comes from here");
        assert_eq!(r.tracks.total, 7);
        assert_eq!(r.albums.offset, 0);
        assert_eq!(r.albums.limit, 0);
    }

    /// The documented LIMIT of the leniency: a non-object top level still fails.
    /// Recorded so the empty-list-on-everything claim is not overstated.
    /// The documented LIMIT of the leniency — MEASURED 2026-08-16, and it is
    /// wider than the contract stated.
    ///
    /// The contract (§2.5) says "a transport error, a non-JSON body and an
    /// invalid top-level shape still FAIL". The first two hold. The third does
    /// not, for ARRAYS: serde's derived `Deserialize` accepts a struct from a
    /// JSON sequence as well as a map, and since both fields carry
    /// `#[serde(default)]` plus a lenient page wrapper, `[]` — and even
    /// `[1, 2]`, whose elements are consumed positionally and swallowed —
    /// deserialize into a perfectly valid EMPTY response.
    ///
    /// What genuinely fails is a SCALAR top level: string, number, bool, null.
    ///
    /// This does not change the contract's conclusion (a JSON-shaped 401/403
    /// body is an object, and it collapses to "you own nothing" exactly as
    /// described) but it does widen the silent-empty surface by one shape, and
    /// on a feature nobody can smoke-test the exact boundary is worth pinning
    /// down rather than approximating.
    #[test]
    fn only_scalar_top_levels_fail_arrays_collapse_to_empty() {
        for ok in ["{}", "[]", "[1,2]"] {
            let parsed = serde_json::from_str::<PurchaseResponse>(ok)
                .unwrap_or_else(|e| panic!("expected {ok} to collapse to empty, got {e}"));
            assert!(parsed.albums.items.is_empty());
            assert!(parsed.tracks.items.is_empty());
        }

        for err in ["\"nope\"", "7", "null", "true"] {
            assert!(
                serde_json::from_str::<PurchaseResponse>(err).is_err(),
                "a scalar top level must still fail: {err}"
            );
        }
    }

    // ── §10-C: the version field that closes the latent #360 regression ───────

    #[test]
    fn purchase_track_carries_a_version_when_the_catalog_supplies_one() {
        let t: PurchaseTrack =
            serde_json::from_str(r#"{"id":1,"title":"Song","version":"Live"}"#).unwrap();
        assert_eq!(t.version.as_deref(), Some("Live"));
    }

    // ── §14.3: goodies must never take an album down ──────────────────────────

    /// The shape of a POPULATED goodies list has never been observed — it comes
    /// back empty on every album nobody owns. So an unexpected shape has to
    /// degrade to "no goodies", not to "this album will not open". `/album/get`
    /// is parsed with the strict structs, which is exactly where that would bite.
    #[test]
    fn a_surprising_goodies_shape_does_not_fail_the_album() {
        for weird in [
            r#"{"id":"1","title":"A","goodies":"a string"}"#,
            r#"{"id":"1","title":"A","goodies":{"unexpected":"object"}}"#,
            r#"{"id":"1","title":"A","goodies":[{"totally":"different"}]}"#,
            r#"{"id":"1","title":"A","goodies":7}"#,
        ] {
            let album: Album = serde_json::from_str(weird)
                .unwrap_or_else(|e| panic!("goodies shape took the album down: {weird} → {e}"));
            assert_eq!(album.title, "A");
        }
    }

    #[test]
    fn a_well_formed_goodie_parses_and_reads_defensively() {
        let album: Album = serde_json::from_str(
            r#"{"id":"1","title":"A","goodies":[
                {"id":5,"name":"Booklet","url":"https://u/b.pdf","original_url":"https://o/b.pdf"}
            ]}"#,
        )
        .unwrap();
        let goodies = album.goodies.expect("goodies present");
        assert_eq!(goodies.len(), 1);
        assert_eq!(goodies[0].best_url(), Some("https://o/b.pdf"));
        assert_eq!(goodies[0].display_name(), "Booklet");
    }
}

#[cfg(test)]
mod upcoming_tests {
    use super::{upcoming_from, Track};
    use chrono::{TimeZone, Utc};

    #[test]
    fn future_streamable_at_wins_over_the_date() {
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).unwrap();
        assert!(upcoming_from(Some(now.timestamp() + 60), None, now));
        assert!(upcoming_from(
            Some(now.timestamp() + 60),
            Some("2000-01-01"),
            now
        ));
        assert!(!upcoming_from(
            Some(now.timestamp() - 60),
            Some("2099-01-01"),
            now
        ));
        // The captured dead track: `streamable_at: null`, past release date.
        assert!(!upcoming_from(None, Some("2013-06-21"), now));
    }

    #[test]
    fn release_date_is_the_fallback_and_absence_is_not_upcoming() {
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).unwrap();
        assert!(upcoming_from(None, Some("2026-10-16"), now));
        assert!(
            !upcoming_from(None, Some("2026-09-02"), now),
            "today is not upcoming"
        );
        assert!(!upcoming_from(None, Some("not a date"), now));
        assert!(!upcoming_from(None, None, now));
    }

    #[test]
    fn a_streamable_track_is_never_upcoming() {
        let track = Track {
            streamable: Some(true),
            release_date_stream: Some("2099-01-01".into()),
            ..Default::default()
        };
        assert!(!track.is_upcoming());
        let track = Track {
            streamable: Some(false),
            release_date_stream: Some("2099-01-01".into()),
            ..Default::default()
        };
        assert!(track.is_upcoming());
    }
}
