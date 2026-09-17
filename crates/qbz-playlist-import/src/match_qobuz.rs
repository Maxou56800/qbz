//! Match imported tracks to Qobuz catalog

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use qbz_models::Track;
use qbz_qobuz::QobuzClient;

use crate::errors::PlaylistImportError;
use crate::models::{ImportProgress, ImportTrack, TrackMatch};
use crate::sink::{ImportEvent, ImportProgressSink};

const SEARCH_LIMIT: u32 = 20;
const TITLE_WEIGHT: f32 = 0.6;
const ARTIST_WEIGHT: f32 = 0.3;
const ALBUM_WEIGHT: f32 = 0.1;
const MIN_SCORE: f32 = 0.65;
const CONCURRENCY: usize = 8;

pub async fn match_tracks(
    client: &QobuzClient,
    tracks: &[ImportTrack],
    progress: Arc<dyn ImportProgressSink>,
) -> Result<Vec<TrackMatch>, PlaylistImportError> {
    let total = tracks.len() as u32;
    let matched_counter = Arc::new(AtomicU32::new(0));
    let completed_counter = Arc::new(AtomicU32::new(0));

    // Pre-allocate results vector with None slots
    let results: Arc<tokio::sync::Mutex<Vec<Option<TrackMatch>>>> =
        Arc::new(tokio::sync::Mutex::new(vec![None; tracks.len()]));

    let owned_tracks: Vec<(usize, ImportTrack)> = tracks
        .iter()
        .enumerate()
        .map(|(i, tr)| (i, tr.clone()))
        .collect();

    stream::iter(owned_tracks)
        .map(|(idx, track)| {
            let client = client.clone();
            let progress = Arc::clone(&progress);
            let matched_counter = Arc::clone(&matched_counter);
            let completed_counter = Arc::clone(&completed_counter);
            let results = Arc::clone(&results);

            async move {
                let query = format!("{} {}", track.artist, track.title);
                let search_result = client.search_tracks(&query, SEARCH_LIMIT, 0, None).await;

                let match_entry = match search_result {
                    Ok(search) => {
                        let (best, score) = select_best_match(&track, &search.items);
                        match best {
                            Some(candidate) if score >= MIN_SCORE => {
                                matched_counter.fetch_add(1, Ordering::Relaxed);
                                TrackMatch {
                                    source: track.clone(),
                                    qobuz_track_id: Some(candidate.id),
                                    qobuz_title: Some(candidate.title.clone()),
                                    qobuz_artist: candidate
                                        .performer
                                        .as_ref()
                                        .map(|a| a.name.clone()),
                                    score,
                                }
                            }
                            _ => TrackMatch {
                                source: track.clone(),
                                qobuz_track_id: None,
                                qobuz_title: None,
                                qobuz_artist: None,
                                score,
                            },
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "Search failed for '{}' - '{}': {}",
                            track.artist,
                            track.title,
                            e
                        );
                        TrackMatch {
                            source: track.clone(),
                            qobuz_track_id: None,
                            qobuz_title: None,
                            qobuz_artist: None,
                            score: 0.0,
                        }
                    }
                };

                // Store result at correct index
                {
                    let mut res = results.lock().await;
                    res[idx] = Some(match_entry);
                }

                let current = completed_counter.fetch_add(1, Ordering::Relaxed) + 1;
                let matched = matched_counter.load(Ordering::Relaxed);

                let current_track = Some(format!("{} - {}", track.artist, track.title));

                progress.emit(ImportEvent::Progress(ImportProgress {
                    phase: "matching".to_string(),
                    current,
                    total,
                    matched_so_far: matched,
                    current_track,
                }));
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<()>>()
        .await;

    // Extract results in order
    let locked = results.lock().await;
    let ordered: Vec<TrackMatch> = locked
        .iter()
        .map(|slot| slot.clone().expect("All slots should be filled"))
        .collect();

    Ok(ordered)
}

fn select_best_match<'a>(track: &ImportTrack, candidates: &'a [Track]) -> (Option<&'a Track>, f32) {
    let mut best: Option<&Track> = None;
    let mut best_score = 0.0f32;
    let mut best_quality = 0.0f32;

    for candidate in candidates {
        // Absence is not unavailability: only an explicit `streamable: false`
        // disqualifies a candidate. Before `Track.streamable` became
        // `Option<bool>`, a `/track/search` payload that omitted the key
        // disqualified EVERY candidate, so the import matched nothing at all —
        // and did it silently, since "no match" is a normal import outcome.
        if !candidate.is_streamable() {
            continue;
        }

        let score = score_candidate(track, candidate);
        let quality = quality_score(candidate);

        if score > best_score + 0.0001 {
            best = Some(candidate);
            best_score = score;
            best_quality = quality;
        } else if (score - best_score).abs() < 0.01 && quality > best_quality {
            best = Some(candidate);
            best_quality = quality;
        }
    }

    (best, best_score)
}

/// The confidence floor [`match_tracks`] gates on, exposed so a UI that shows
/// the RANKED list can label a weak candidate instead of silently offering it
/// as if it were a match. Nothing here applies it — the importer still owns
/// that decision, and the replacement flow deliberately shows sub-threshold
/// rows because a human confirms the swap.
pub const MIN_MATCH_SCORE: f32 = MIN_SCORE;

/// Every candidate for `track`, best first — the ordering [`select_best_match`]
/// picks its winner out of, handed over whole instead of collapsed to one row.
///
/// This exists for the "find an available version" flow of a track Qobuz pulled
/// from the catalogue. There a HUMAN confirms the swap, so the surface needs the
/// order and every candidate's score, not just the winner; the reference (Tauri)
/// showed Qobuz's raw relevance order with no scoring at all, which is what this
/// replaces.
///
/// Purely ADDITIVE by construction: [`match_tracks`] and [`select_best_match`]
/// are untouched, so the importer's behaviour cannot move because of it.
///
/// The same three rules as `select_best_match`, in the same order, deliberately:
///
///  1. Non-streamable candidates are DROPPED, not ranked low. Offering a
///     replacement that is itself unplayable is the one outcome the whole
///     feature exists to prevent.
///  2. The score is [`score_candidate`] verbatim — the ISRC short-circuit to
///     1.0 first, then title 0.6 / artist 0.3 / album 0.1 plus the duration
///     bonus.
///  3. Equal scores are broken by [`quality_score`] (bit depth first, sample
///     rate second). That is the owner's *"eliminan una calidad y agregan
///     otra"* case: the same recording re-published one tier up or down.
///
/// # Why the score is BUCKETED rather than compared with a tolerance
///
/// `select_best_match` treats scores within `0.01` as equal. Written directly
/// into a comparator that window is NOT transitive (a≈b, b≈c, a<c), and since
/// Rust 1.81 `slice::sort_by` may PANIC on a comparator that does not implement
/// a total order. Rounding to hundredths first is the same window expressed as
/// a key, so the ordering is total and the head of this list is still the track
/// `select_best_match` would have returned.
///
/// The sort is STABLE, so candidates tied on both score and quality keep
/// Qobuz's own relevance order — the reference's ordering, kept as the last
/// tiebreak instead of being scrambled.
pub fn rank_candidates(track: &ImportTrack, candidates: &[Track]) -> Vec<(Track, f32)> {
    let mut ranked: Vec<(Track, f32)> = candidates
        .iter()
        .filter(|candidate| candidate.is_streamable())
        .map(|candidate| (candidate.clone(), score_candidate(track, candidate)))
        .collect();

    // The 0.01 equality window of `select_best_match`, as a transitive key.
    fn bucket(score: f32) -> i32 {
        (score * 100.0).round() as i32
    }

    ranked.sort_by(|(a_track, a_score), (b_track, b_score)| {
        bucket(*b_score)
            .cmp(&bucket(*a_score))
            .then_with(|| quality_score(b_track).total_cmp(&quality_score(a_track)))
    });
    ranked
}

fn score_candidate(track: &ImportTrack, candidate: &Track) -> f32 {
    if let (Some(isrc), Some(candidate_isrc)) = (&track.isrc, &candidate.isrc) {
        if isrc.eq_ignore_ascii_case(candidate_isrc) {
            return 1.0;
        }
    }

    let title_score = similarity(&track.title, &candidate.title);
    let artist_score = similarity(
        &track.artist,
        candidate
            .performer
            .as_ref()
            .map(|a| a.name.as_str())
            .unwrap_or(""),
    );
    let album_score = track
        .album
        .as_ref()
        .map(|album| {
            candidate
                .album
                .as_ref()
                .map(|a| similarity(album, &a.title))
                .unwrap_or(0.0)
        })
        .unwrap_or(0.0);

    let mut score =
        title_score * TITLE_WEIGHT + artist_score * ARTIST_WEIGHT + album_score * ALBUM_WEIGHT;

    if let (Some(import_duration), Some(candidate_duration)) = (
        track.duration_ms,
        Some((candidate.duration as u64).saturating_mul(1000)),
    ) {
        let diff = if import_duration > candidate_duration {
            import_duration - candidate_duration
        } else {
            candidate_duration - import_duration
        };

        if diff <= 3000 {
            score += 0.05;
        } else if diff <= 5000 {
            score += 0.02;
        }
    }

    score
}

fn similarity(a: &str, b: &str) -> f32 {
    let na = normalize(a);
    let nb = normalize(b);

    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }

    if na == nb {
        return 1.0;
    }

    if na.contains(&nb) || nb.contains(&na) {
        return 0.85;
    }

    token_overlap(&na, &nb)
}

fn normalize(input: &str) -> String {
    let stripped = remove_bracketed(input);
    let mut cleaned = String::new();

    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() || ch.is_whitespace() {
            cleaned.push(ch.to_ascii_lowercase());
        } else {
            cleaned.push(' ');
        }
    }

    let stop_words = [
        "remaster",
        "remastered",
        "deluxe",
        "edition",
        "live",
        "feat",
        "featuring",
        "version",
        "mix",
        "mono",
        "stereo",
        "edit",
    ];

    cleaned
        .split_whitespace()
        .filter(|token| !stop_words.contains(token))
        .collect::<Vec<_>>()
        .join(" ")
}

fn remove_bracketed(input: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;

    for ch in input.chars() {
        match ch {
            '(' | '[' => {
                depth += 1;
            }
            ')' | ']' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            _ => {
                if depth == 0 {
                    out.push(ch);
                }
            }
        }
    }

    out
}

fn token_overlap(a: &str, b: &str) -> f32 {
    let a_tokens: Vec<&str> = a.split_whitespace().collect();
    let b_tokens: Vec<&str> = b.split_whitespace().collect();

    if a_tokens.is_empty() || b_tokens.is_empty() {
        return 0.0;
    }

    let mut matches = 0u32;
    for token in &a_tokens {
        if b_tokens.contains(token) {
            matches += 1;
        }
    }

    matches as f32 / a_tokens.len().max(b_tokens.len()) as f32
}

fn quality_score(track: &Track) -> f32 {
    let bit_depth = track.maximum_bit_depth.unwrap_or(0) as f32;
    let sample_rate = track.maximum_sampling_rate.unwrap_or(0.0) as f32;
    bit_depth * 100000.0 + sample_rate
}

#[cfg(test)]
mod tests {
    use super::*;
    use qbz_models::{AlbumSummary, Artist, ImageSet};

    fn import_track(title: &str, artist: &str) -> ImportTrack {
        ImportTrack {
            title: title.to_string(),
            artist: artist.to_string(),
            album: None,
            duration_ms: None,
            isrc: None,
            provider_id: None,
            provider_url: None,
        }
    }

    fn qobuz_track(id: u64, title: &str, artist: &str) -> Track {
        Track {
            favorited_at: None,
            purchasable: None,
            id,
            title: title.to_string(),
            version: None,
            work: None,
            isrc: None,
            audio_info: None,
            duration: 0,
            track_number: 0,
            media_number: None,
            performer: Some(Artist {
                name: artist.to_string(),
                ..Artist::default()
            }),
            album: None,
            hires: false,
            hires_streamable: false,
            maximum_sampling_rate: None,
            maximum_bit_depth: None,
            streamable: Some(true),
            streamable_at: None,
            release_date_stream: None,
            parental_warning: false,
            playlist_track_id: None,
            performers: None,
            composer: None,
            copyright: None,
        }
    }

    fn album_summary(title: &str) -> AlbumSummary {
        AlbumSummary {
            id: String::new(),
            title: title.to_string(),
            image: ImageSet::default(),
            label: None,
            genre: None,
            artist: None,
            version: None,
            streamable: None,
            streamable_at: None,
            release_date_stream: None,
            release_date_original: None,
        }
    }

    // ── normalize / remove_bracketed / token_overlap ──

    #[test]
    fn normalize_strips_brackets_and_stop_words() {
        assert_eq!(normalize("Song Title (Remastered 2011)"), "song title");
        assert_eq!(normalize("Song Title [Deluxe Edition]"), "song title");
        assert_eq!(normalize("Hey Jude - Remastered"), "hey jude");
    }

    #[test]
    fn normalize_handles_nested_brackets() {
        assert_eq!(normalize("Title (Live [2020] Version)"), "title");
    }

    #[test]
    fn normalize_degrades_non_ascii_to_spaces() {
        // Accented/CJK chars are not ASCII-alphanumeric → replaced by spaces.
        // Locks the (lossy) accented-title behavior of the Tauri original.
        assert_eq!(normalize("Café Tacvba"), "caf tacvba");
        assert_eq!(normalize("너의 의미"), "");
    }

    #[test]
    fn normalize_lowercases_and_collapses_punctuation() {
        assert_eq!(normalize("AC/DC - T.N.T."), "ac dc t n t");
    }

    #[test]
    fn remove_bracketed_basic_and_nested() {
        assert_eq!(remove_bracketed("a (b) c"), "a  c");
        assert_eq!(remove_bracketed("a (b [c] d) e"), "a  e");
        assert_eq!(remove_bracketed("no brackets"), "no brackets");
        // Unbalanced closers are ignored at depth 0
        assert_eq!(remove_bracketed(") leading"), " leading");
    }

    #[test]
    fn token_overlap_ratio_uses_longer_side() {
        assert_eq!(token_overlap("a b", "a b"), 1.0);
        assert_eq!(token_overlap("a b", "a b c d"), 0.5);
        assert_eq!(token_overlap("x", "y"), 0.0);
        assert_eq!(token_overlap("", "a"), 0.0);
    }

    // ── similarity ──

    #[test]
    fn similarity_exact_after_normalization_is_one() {
        assert_eq!(similarity("Hey Jude (Remastered)", "hey jude"), 1.0);
    }

    #[test]
    fn similarity_substring_is_085() {
        assert_eq!(similarity("hey jude", "hey jude na na"), 0.85);
    }

    #[test]
    fn similarity_falls_back_to_token_overlap() {
        // "hey there" vs "jude there": 1 shared token / max(2, 2) = 0.5
        assert_eq!(similarity("hey there", "jude there"), 0.5);
    }

    #[test]
    fn similarity_empty_is_zero() {
        assert_eq!(similarity("", "anything"), 0.0);
        assert_eq!(similarity("anything", ""), 0.0);
    }

    // ── score_candidate ──

    #[test]
    fn score_candidate_isrc_short_circuit_is_case_insensitive() {
        let mut source = import_track("Completely Different", "Nobody");
        source.isrc = Some("uskO11600123".to_string());
        let mut candidate = qobuz_track(1, "Other Title", "Other Artist");
        candidate.isrc = Some("USKO11600123".to_string());
        assert_eq!(score_candidate(&source, &candidate), 1.0);
    }

    #[test]
    fn score_candidate_weights_title_artist_album() {
        let mut source = import_track("hey jude", "the beatles");
        source.album = Some("past masters".to_string());
        let mut candidate = qobuz_track(1, "hey jude", "the beatles");
        candidate.album = Some(album_summary("past masters"));
        // duration_ms is None on the source → no duration bonus, so the score
        // is exactly the 0.6/0.3/0.1 weighted sum (all components 1.0 here).
        let score = score_candidate(&source, &candidate);
        assert!((score - 1.0).abs() < 1e-6, "got {}", score);
    }

    #[test]
    fn score_candidate_duration_bonus_tiers() {
        let mut source = import_track("hey jude", "the beatles");
        let mut candidate = qobuz_track(1, "hey jude", "the beatles");
        candidate.duration = 200; // 200_000 ms

        // No source duration → no bonus (source-duration-only quirk).
        let base = score_candidate(&source, &candidate);
        assert!((base - 0.9).abs() < 1e-6, "got {}", base);

        // Within 3s → +0.05
        source.duration_ms = Some(202_000);
        let close = score_candidate(&source, &candidate);
        assert!((close - 0.95).abs() < 1e-6, "got {}", close);

        // Within 5s → +0.02
        source.duration_ms = Some(204_500);
        let near = score_candidate(&source, &candidate);
        assert!((near - 0.92).abs() < 1e-6, "got {}", near);

        // Beyond 5s → no bonus
        source.duration_ms = Some(210_000);
        let far = score_candidate(&source, &candidate);
        assert!((far - 0.9).abs() < 1e-6, "got {}", far);
    }

    // ── select_best_match ──

    #[test]
    fn select_best_match_skips_non_streamable() {
        let source = import_track("hey jude", "the beatles");
        let mut perfect = qobuz_track(1, "hey jude", "the beatles");
        // Explicit `Some(false)` — the skip must fire on the server SAYING no,
        // never on a payload that simply stayed quiet (see `is_streamable`).
        perfect.streamable = Some(false);
        let weaker = qobuz_track(2, "hey jude na na", "the beatles");

        let candidates = [perfect, weaker];
        let (best, _score) = select_best_match(&source, &candidates);
        assert_eq!(best.map(|t| t.id), Some(2));
    }

    #[test]
    fn select_best_match_score_below_min_score_threshold() {
        // select_best_match does NOT gate on MIN_SCORE — the CALLER
        // (match_tracks) drops sub-threshold matches. Prove a PARTIAL match
        // (same artist, different title → ~0.3, well under 0.65) is still
        // RETURNED, leaving the rejection to the caller. NOTE: it must be a
        // partial, not pure junk — a fully disjoint candidate scores 0.0,
        // never beats the 0.0 seed (`score > best_score + 0.0001`), and comes
        // back as None, which is a different code path.
        let source = import_track("hey jude", "the beatles");
        let partial = qobuz_track(1, "let it be", "the beatles");
        let candidates = [partial];
        let (best, score) = select_best_match(&source, &candidates);
        assert!(best.is_some(), "a partial match must still be returned");
        assert!(score > 0.0 && score < MIN_SCORE, "got {}", score);
    }

    #[test]
    fn select_best_match_hi_res_tiebreak_within_001() {
        let source = import_track("hey jude", "the beatles");
        let mut cd = qobuz_track(1, "hey jude", "the beatles");
        cd.maximum_bit_depth = Some(16);
        cd.maximum_sampling_rate = Some(44.1);
        let mut hires = qobuz_track(2, "hey jude", "the beatles");
        hires.maximum_bit_depth = Some(24);
        hires.maximum_sampling_rate = Some(192.0);

        // Equal scores → quality_score decides, regardless of order.
        let ordered = [cd.clone(), hires.clone()];
        let (best, _) = select_best_match(&source, &ordered);
        assert_eq!(best.map(|t| t.id), Some(2));
        let reversed = [hires, cd];
        let (best, _) = select_best_match(&source, &reversed);
        assert_eq!(best.map(|t| t.id), Some(2));
    }

    #[test]
    fn select_best_match_empty_candidates() {
        let source = import_track("hey jude", "the beatles");
        let (best, score) = select_best_match(&source, &[]);
        assert!(best.is_none());
        assert_eq!(score, 0.0);
    }

    // ── rank_candidates ──

    #[test]
    fn rank_candidates_drops_non_streamable() {
        // The dropped one is the PERFECT match, so a rank that merely scored it
        // low would still surface it first — only an actual drop passes.
        let source = import_track("hey jude", "the beatles");
        let mut dead = qobuz_track(1, "hey jude", "the beatles");
        dead.streamable = Some(false);
        let live = qobuz_track(2, "hey jude na na", "the beatles");

        let ranked = rank_candidates(&source, &[dead, live]);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0.id, 2);
    }

    #[test]
    fn rank_candidates_head_is_select_best_match() {
        // The contract that lets the modal preselect row 0: the head of the
        // ranked list IS the winner the importer would have taken.
        let source = import_track("hey jude", "the beatles");
        let weak = qobuz_track(1, "let it be", "the beatles");
        let strong = qobuz_track(2, "hey jude", "the beatles");

        let candidates = [weak, strong];
        let (best, _) = select_best_match(&source, &candidates);
        let ranked = rank_candidates(&source, &candidates);
        assert_eq!(ranked[0].0.id, best.map(|t| t.id).unwrap());
        assert!(ranked[0].1 > ranked[1].1);
    }

    #[test]
    fn rank_candidates_isrc_hit_leads_with_score_one() {
        // The "same recording, new album id" case: a candidate whose TEXT is
        // wrong but whose ISRC matches must still lead at 1.0.
        let mut source = import_track("Completely Different", "Nobody");
        source.isrc = Some("uskO11600123".to_string());
        let mut relink = qobuz_track(1, "Other Title", "Other Artist");
        relink.isrc = Some("USKO11600123".to_string());
        let decoy = qobuz_track(2, "Completely Different", "Nobody");

        let ranked = rank_candidates(&source, &[decoy, relink]);
        assert_eq!(ranked[0].0.id, 1);
        assert_eq!(ranked[0].1, 1.0);
    }

    #[test]
    fn rank_candidates_equal_scores_prefer_the_better_tier() {
        // The owner's "eliminan una calidad y agregan otra" case, and the reason
        // the score is bucketed: these two score identically.
        let source = import_track("hey jude", "the beatles");
        let mut cd = qobuz_track(1, "hey jude", "the beatles");
        cd.maximum_bit_depth = Some(16);
        cd.maximum_sampling_rate = Some(44.1);
        let mut hires = qobuz_track(2, "hey jude", "the beatles");
        hires.maximum_bit_depth = Some(24);
        hires.maximum_sampling_rate = Some(192.0);

        let ranked = rank_candidates(&source, &[cd, hires]);
        assert_eq!(ranked[0].0.id, 2);
    }

    #[test]
    fn rank_candidates_empty_is_empty_not_a_panic() {
        let source = import_track("hey jude", "the beatles");
        assert!(rank_candidates(&source, &[]).is_empty());
    }

    // ── quality_score ──

    #[test]
    fn quality_score_weighs_bit_depth_over_sample_rate() {
        let mut a = qobuz_track(1, "x", "y");
        a.maximum_bit_depth = Some(24);
        a.maximum_sampling_rate = Some(44.1);
        let mut b = qobuz_track(2, "x", "y");
        b.maximum_bit_depth = Some(16);
        b.maximum_sampling_rate = Some(192.0);
        assert!(quality_score(&a) > quality_score(&b));

        let none = qobuz_track(3, "x", "y");
        assert_eq!(quality_score(&none), 0.0);
    }
}
