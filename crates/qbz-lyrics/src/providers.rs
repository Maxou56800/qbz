//! External fallback lyrics providers.
//!
//! Ported VERBATIM from `src-tauri/src/lyrics/providers.rs` (request shapes
//! byte-identical: endpoints, query params, User-Agent, 10s timeout). The
//! only change is `eprintln!`/`println!` -> `log::` macros (crate
//! convention); the transport/miss contract (`Ok(None)` vs `Err`) is
//! unchanged — only `Err` triggers the orchestrator's single retry.

use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use urlencoding::encode;

use crate::model::{normalize, LyricsProvider};

/// Build a shared HTTP client with reasonable timeout
fn build_client() -> Result<Client, String> {
    qbz_net_proxy::apply_current(Client::builder())
        .map_err(|e| format!("Failed to apply proxy configuration: {}", e))?
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))
}

#[derive(Debug, Clone)]
pub struct LyricsData {
    pub plain: Option<String>,
    pub synced_lrc: Option<String>,
    pub provider: LyricsProvider,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) struct LrclibItem {
    #[serde(default)]
    pub track_name: String,
    #[serde(default)]
    pub artist_name: String,
    pub album_name: Option<String>,
    pub duration: Option<f64>,
    pub instrumental: Option<bool>,
    pub plain_lyrics: Option<String>,
    pub synced_lyrics: Option<String>,
}

/// Fetch lyrics from LRCLIB (search-first, GET as fallback).
///
/// Search returns multiple candidates so we can pick the one with synced
/// lyrics. GET is only used as fallback when search returns nothing.
///
/// Returns:
/// - `Ok(Some(data))` — lyrics found
/// - `Ok(None)` — not found (API responded but no match)
/// - `Err(msg)` — network/transport error (caller should retry)
pub async fn fetch_lrclib(
    title: &str,
    artist: &str,
    duration_secs: Option<u64>,
) -> Result<Option<LyricsData>, String> {
    let client = build_client()?;

    let mut had_network_error = false;

    // Search first — returns multiple candidates, pick_best_match prioritises synced
    let results = match fetch_lrclib_search(&client, title, artist).await {
        Ok(items) => items,
        Err(e) => {
            log::warn!("[Lyrics] LRCLIB search failed (will try GET): {}", e);
            had_network_error = true;
            Vec::new()
        }
    };

    let mut best = pick_best_match(&results, title, artist, duration_secs);

    // Fall back to exact-match GET only when search yielded nothing
    if best.is_none() {
        match fetch_lrclib_get(&client, title, artist).await {
            Ok(item) => best = item,
            Err(e) => {
                log::warn!("[Lyrics] LRCLIB GET fallback failed: {}", e);
                had_network_error = true;
            }
        }
    }

    let Some(item) = best else {
        if had_network_error {
            return Err("LRCLIB requests failed due to network errors".to_string());
        }
        return Ok(None);
    };

    if item.instrumental.unwrap_or(false) {
        return Ok(None);
    }

    let plain = item.plain_lyrics.and_then(clean_lyrics);
    let synced = item.synced_lyrics.and_then(clean_lyrics);

    if plain.is_none() && synced.is_none() {
        return Ok(None);
    }

    Ok(Some(LyricsData {
        plain,
        synced_lrc: synced,
        provider: LyricsProvider::Lrclib,
    }))
}

pub async fn fetch_lyrics_ovh(title: &str, artist: &str) -> Option<LyricsData> {
    let client = match build_client() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[Lyrics] {}", e);
            return None;
        }
    };

    let artist_encoded = encode(artist);
    let title_encoded = encode(title);
    let url = format!(
        "https://api.lyrics.ovh/v1/{}/{}",
        artist_encoded, title_encoded
    );

    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[Lyrics] lyrics.ovh request failed: {}", e);
            return None;
        }
    };

    if !response.status().is_success() {
        return None;
    }

    #[derive(Deserialize)]
    struct OvhResponse {
        lyrics: Option<String>,
    }

    let data: OvhResponse = match response.json().await {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[Lyrics] lyrics.ovh response parse failed: {}", e);
            return None;
        }
    };

    let plain = data.lyrics.and_then(clean_lyrics);
    plain.as_ref()?;

    Some(LyricsData {
        plain,
        synced_lrc: None,
        provider: LyricsProvider::Ovh,
    })
}

async fn fetch_lrclib_get(
    client: &Client,
    title: &str,
    artist: &str,
) -> Result<Option<LrclibItem>, String> {
    let response = client
        .get("https://lrclib.net/api/get")
        .header("User-Agent", "QBZ-Nix/1.0 (https://github.com/qbz-nix)")
        .query(&[("track_name", title), ("artist_name", artist)])
        .send()
        .await
        .map_err(|e| format!("LRCLIB get request failed: {}", e))?;

    if !response.status().is_success() {
        log::debug!("[Lyrics] LRCLIB get returned status: {}", response.status());
        return Ok(None);
    }

    // Get raw text first for debugging
    let text = response
        .text()
        .await
        .map_err(|e| format!("LRCLIB get response text failed: {}", e))?;

    // Log if syncedLyrics is present in raw response
    let has_synced = text.contains("syncedLyrics") && !text.contains("\"syncedLyrics\":null");
    log::debug!(
        "[Lyrics] LRCLIB get raw response has syncedLyrics: {}, len: {}",
        has_synced,
        text.len()
    );

    let item: LrclibItem = serde_json::from_str(&text)
        .map_err(|e| format!("LRCLIB get response parse failed: {}", e))?;

    log::debug!(
        "[Lyrics] LRCLIB parsed - synced_lyrics present: {}",
        item.synced_lyrics.is_some()
    );

    Ok(Some(item))
}

async fn fetch_lrclib_search(
    client: &Client,
    title: &str,
    artist: &str,
) -> Result<Vec<LrclibItem>, String> {
    let response = client
        .get("https://lrclib.net/api/search")
        .header("User-Agent", "QBZ-Nix/1.0 (https://github.com/qbz-nix)")
        .query(&[("track_name", title), ("artist_name", artist)])
        .send()
        .await
        .map_err(|e| format!("LRCLIB search request failed: {}", e))?;

    if !response.status().is_success() {
        return Ok(Vec::new());
    }

    let items: Vec<LrclibItem> = response
        .json()
        .await
        .map_err(|e| format!("LRCLIB search response parse failed: {}", e))?;

    Ok(items)
}

pub(crate) fn pick_best_match(
    items: &[LrclibItem],
    title: &str,
    artist: &str,
    duration_secs: Option<u64>,
) -> Option<LrclibItem> {
    let normalized_title = normalize(title);
    let normalized_artist = normalize(artist);
    let target_duration = duration_secs.unwrap_or(0) as f64;

    let mut best: Option<(i32, &LrclibItem)> = None;

    for item in items {
        let item_title = normalize(&item.track_name);
        let item_artist = normalize(&item.artist_name);

        let mut score = 0;

        if item_title == normalized_title {
            score += 3;
        }
        if item_artist == normalized_artist {
            score += 3;
        }
        if item_title == normalized_title && item_artist == normalized_artist {
            score += 4;
        }

        if let Some(duration) = item.duration {
            if target_duration > 0.0 {
                let diff = (duration - target_duration).abs();
                if diff <= 2.0 {
                    score += 3;
                } else if diff <= 5.0 {
                    score += 1;
                }
            }
        }

        if item
            .synced_lyrics
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            score += 2;
        } else if item
            .plain_lyrics
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            score += 1;
        }

        match best {
            Some((best_score, _)) if score <= best_score => {}
            _ => best = Some((score, item)),
        }
    }

    best.map(|(_, item)| item.clone())
}

fn clean_lyrics(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        track: &str,
        artist: &str,
        duration: Option<f64>,
        synced: Option<&str>,
        plain: Option<&str>,
    ) -> LrclibItem {
        LrclibItem {
            track_name: track.to_string(),
            artist_name: artist.to_string(),
            album_name: None,
            duration,
            instrumental: None,
            plain_lyrics: plain.map(str::to_string),
            synced_lyrics: synced.map(str::to_string),
        }
    }

    // Tauri's providers.rs ships no tests — this is the scorer matrix
    // written from the documented weights (review §1.3): title==+3,
    // artist==+3, both +4 extra (exact pair = 10), duration |Δ|<=2s +3 /
    // <=5s +1, synced +2 / plain +1, ties keep the earlier candidate.

    #[test]
    fn exact_pair_beats_partial_matches() {
        let items = vec![
            item("Song", "Other Artist", None, Some("[00:01.00]x"), None), // 3+2=5
            item("Song", "Artist", None, None, Some("text")),              // 3+3+4+1=11
            item("Other Song", "Artist", None, Some("[00:01.00]x"), None), // 3+2=5
        ];
        let best = pick_best_match(&items, "Song", "Artist", None).unwrap();
        assert_eq!(best.track_name, "Song");
        assert_eq!(best.artist_name, "Artist");
    }

    #[test]
    fn duration_tiers_score_3_then_1() {
        // Same title/artist; only duration differs. <=2s diff (+3) must beat
        // <=5s diff (+1) and >5s (+0).
        let items = vec![
            item("S", "A", Some(210.0), None, Some("p")), // diff 10 -> +0
            item("S", "A", Some(204.0), None, Some("p")), // diff 4 -> +1
            item("S", "A", Some(201.0), None, Some("p")), // diff 1 -> +3
        ];
        let best = pick_best_match(&items, "S", "A", Some(200)).unwrap();
        assert_eq!(best.duration, Some(201.0));

        // Without a target duration there is no duration scoring at all:
        // ties keep the earlier candidate.
        let best = pick_best_match(&items, "S", "A", None).unwrap();
        assert_eq!(best.duration, Some(210.0));
    }

    #[test]
    fn synced_outranks_plain() {
        let items = vec![
            item("S", "A", None, None, Some("plain text")),    // +1
            item("S", "A", None, Some("[00:01.00]hi"), None),  // +2
        ];
        let best = pick_best_match(&items, "S", "A", None).unwrap();
        assert!(best.synced_lyrics.is_some());

        // Whitespace-only synced does NOT earn the synced bonus.
        let items = vec![
            item("S", "A", None, Some("   "), Some("plain")), // plain bonus +1
            item("S", "A", None, None, Some("plain")),        // +1, tie -> earlier wins
        ];
        let best = pick_best_match(&items, "S", "A", None).unwrap();
        assert_eq!(best.synced_lyrics.as_deref(), Some("   "));
    }

    #[test]
    fn ties_keep_the_earlier_candidate() {
        let items = vec![
            item("S", "A", None, None, Some("first")),
            item("S", "A", None, None, Some("second")),
        ];
        let best = pick_best_match(&items, "S", "A", None).unwrap();
        assert_eq!(best.plain_lyrics.as_deref(), Some("first"));
    }

    #[test]
    fn normalization_is_lowercase_whitespace_collapse() {
        let items = vec![item("  MY   song ", "THE artist", None, None, Some("p"))];
        let best = pick_best_match(&items, "my song", "the  ARTIST", None).unwrap();
        assert_eq!(best.plain_lyrics.as_deref(), Some("p"));
        // Sanity: that match scored as the exact pair (10 + plain 1), not 0 —
        // a non-matching item would still be returned (best of one), so
        // verify against a competitor.
        let items = vec![
            item("unrelated", "nobody", None, Some("[00:01.00]x"), None), // +2
            item("  MY   song ", "THE artist", None, None, Some("p")),    // 11
        ];
        let best = pick_best_match(&items, "my song", "the artist", None).unwrap();
        assert_eq!(best.plain_lyrics.as_deref(), Some("p"));
    }

    #[test]
    fn empty_candidate_list_yields_none() {
        assert!(pick_best_match(&[], "S", "A", None).is_none());
    }

    #[test]
    fn clean_lyrics_trims_and_drops_whitespace_only() {
        assert_eq!(clean_lyrics("  hi  ".into()).as_deref(), Some("hi"));
        assert!(clean_lyrics("   \n\t ".into()).is_none());
        assert!(clean_lyrics(String::new()).is_none());
    }
}
