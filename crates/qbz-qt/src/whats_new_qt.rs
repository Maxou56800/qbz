//! What's New modal controller + release-notes markdown renderer — the Qt port
//! of `crates/qbz/src/whats_new.rs`.
//!
//! On open, fetches the GitHub release whose tag matches the running version
//! (`https://api.github.com/repos/vicrodh/qbz/releases/tags/v{version}`) off
//! the UI thread, parses its markdown `body` into a flat block model, and
//! publishes it as `QbzAbout.whatsNewJson`. There is NO on-disk cache and no
//! retry: closing and reopening re-fetches, exactly like the reference.
//!
//! # THE MARKDOWN SUBSET IS NOT MARKDOWN
//!
//! [`render_markdown`] is a line-for-line transcription of
//! `whats_new.rs:337-412`, which is itself a 1:1 port of the Tauri
//! `renderMarkdownWithToc`. Its rules are surprising ON PURPOSE and must not be
//! "improved":
//!
//! - `#` and `##` collapse to the SAME level (0) and both push a TOC entry;
//!   `###` is level 1 and pushes nothing.
//! - An **indent-0 `- item` is NOT a bullet** — it becomes a level-0 SECTION
//!   plus a TOC entry. This is the rule a naive renderer always gets wrong.
//! - Bullet level is `floor(leading_spaces / 2)`, a tab counting as 2.
//! - `**` and backticks are STRIPPED, not styled; inline `[text](url)` is
//!   reduced to `text`. There is no bold, italic, inline code or inline link
//!   anywhere in the rendered body.
//! - A bullet or paragraph that is ENTIRELY one markdown link becomes a
//!   clickable `KIND_LINK` block — that is how TAG_DETAILS.md's final
//!   full-changelog line renders as an accent row.
//!
//! `Text.MarkdownText` is a DIFFERENT subset and was deliberately not used: it
//! would style the markers the reference strips, bullet the indent-0 items the
//! reference promotes to sections, and render links inline. Off the same source
//! it would produce a visibly different document from the Slint app and from
//! the website.
//!
//! # Divergences from the reference
//!
//! - `release_date_iso` keeps only the civil date; QML formats it (`qbz-qt` has no chrono
//!   dependency and is not gaining one for a date the GitHub API always emits
//!   in a fixed RFC3339 shape). Same output, same raw-string fallback.
//! - The blocks are plain `Serialize` structs instead of Slint model rows; the
//!   field names match `WhatsNewState` (`state.slint:6141-6149`) so both
//!   frontends read one document shape.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use cxx_qt_lib::QString;
use serde::{Deserialize, Serialize};

use crate::about_bridge::ui;

const GITHUB_RELEASES_URL: &str = "https://api.github.com/repos/vicrodh/qbz/releases";

/// Block kinds, shared with the Slint model (`whats_new.rs:24-28`).
const KIND_SECTION: i32 = 0;
const KIND_BULLET: i32 = 1;
const KIND_PARAGRAPH: i32 = 2;
/// A whole-line markdown link `[text](url)` — rendered as a clickable link.
const KIND_LINK: i32 = 3;

/// GitHub release JSON (only the fields the modal needs).
#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    published_at: String,
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The parsed release the controller applies to the document.
struct FetchedRelease {
    version: String,
    date: String,
    body: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: i32,
    pub level: i32,
    pub text: String,
    pub id: String,
    pub url: String,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub id: String,
    pub label: String,
}

/// One curated card of the release's visual deck.
///
/// The deck is what the modal shows FIRST; the rendered release body becomes
/// its last card. Text lives here and not in the artwork on purpose: a msgid is
/// the English string and travels to all eight catalogs, while text baked into
/// a `.webp` would need eight sets of images per release.
///
/// `action_section` is a `SettingsView` sub-section index (Audio 0, Playback 1,
/// Appearance 2 — the order that file documents), used as
/// `QbzBridge.settingsSetSection(n)` before navigating. `-1` opens Settings
/// without choosing a section; `None` means the card has no button.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Slide {
    pub image: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "actionLabel")]
    pub action_label: String,
    #[serde(rename = "actionSection")]
    pub action_section: i32,
}

/// The deck for `version`, empty when that version has none.
///
/// Matched on the `MAJOR.MINOR.PATCH` prefix so a `-rc`/`-beta` build of the
/// same version still shows its deck. A version without a deck degrades to the
/// release-notes body the modal has always rendered.
fn deck_for(version: &str) -> Vec<Slide> {
    let base = version
        .split(['-', '+'])
        .next()
        .unwrap_or(version)
        .to_string();
    if base != "2.1.2" {
        return Vec::new();
    }
    let art = |file: &str| format!("../assets/whatsnew/2.1.2/{file}");
    vec![
        Slide {
            image: art("01-settings.webp"),
            title: qbz_i18n::t("Reorganised settings"),
            body: qbz_i18n::t(
                "I know that listening habits and preferences vary as much as musical tastes do, so the options have been reorganised and the less frequently used ones have been hidden. Have a look at the settings section and customise it to your liking.",
            ),
            action_label: qbz_i18n::t("Open settings"),
            action_section: -1,
        },
        Slide {
            image: art("02-memory.webp"),
            title: qbz_i18n::t("Cache profiles"),
            body: qbz_i18n::t(
                "Caching is no longer a product-driven decision; you can now choose whether you prefer the cache, memory or storage to handle it, depending on your preferences and/or your computer's capabilities.",
            ),
            action_label: qbz_i18n::t("Open playback"),
            action_section: 1,
        },
        Slide {
            image: art("03-loudness.webp"),
            title: qbz_i18n::t("Volume normalization"),
            body: qbz_i18n::t(
                "The feature has been completely rewritten; we now utilise the LUFS information provided by Qobuz and have implemented profiles that comply with industry standards.",
            ),
            action_label: qbz_i18n::t("Open audio"),
            action_section: 0,
        },
        Slide {
            image: art("04-qol.webp"),
            title: qbz_i18n::t("Quality of Life"),
            body: qbz_i18n::t(
                "Features that were lost during the front-end changes have been restored, along with more and better batch actions, additional playback controls (jump forward and back within 10 seconds on the seek bar, loop playback), etc.",
            ),
            action_label: qbz_i18n::t("Open settings"),
            action_section: -1,
        },
        Slide {
            image: art("05-connect.webp"),
            title: qbz_i18n::t("QConnect hardening"),
            body: qbz_i18n::t(
                "More stable performance, particularly bug fixes, issues that were preventing an official-style implementation, mismatched queues, and crashes. This feature is undergoing continuous improvement.",
            ),
            action_label: String::new(),
            action_section: -1,
        },
    ]
}

/// Pref key: the last version whose deck was dismissed. The deck opens by
/// itself exactly once per version; the hamburger row reopens it any time.
const SEEN_PREF: &str = "whatsnew_seen_version";

#[derive(Default)]
struct State {
    version: String,
    date: String,
    has_body: bool,
    blocks: Vec<Block>,
    toc: Vec<TocEntry>,
}

static OPEN: AtomicBool = AtomicBool::new(false);
static LOADING: AtomicBool = AtomicBool::new(false);
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

#[derive(Serialize)]
struct Doc<'a> {
    open: bool,
    loading: bool,
    version: &'a str,
    date: &'a str,
    #[serde(rename = "hasBody")]
    has_body: bool,
    toc: &'a [TocEntry],
    blocks: &'a [Block],
    deck: &'a [Slide],
}

pub fn publish() {
    let st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    // The deck is keyed on the RUNNING version, never on the fetched release:
    // the artwork ships inside this binary, so a fetch that lands on another
    // tag (or fails) must not pair these cards with another version's notes.
    let deck = deck_for(crate::about_qt::app_version());
    let doc = Doc {
        open: OPEN.load(Ordering::SeqCst),
        loading: LOADING.load(Ordering::SeqCst),
        version: &st.version,
        date: &st.date,
        has_body: st.has_body,
        toc: &st.toc,
        blocks: &st.blocks,
        deck: &deck,
    };
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into());
    drop(st);
    ui(move |mut b| b.as_mut().set_whats_new_json(QString::from(json.as_str())));
}

/// Show the modal in its loading state, then fetch + render off the UI thread.
pub fn open() {
    let version = crate::about_qt::app_version().to_string();
    OPEN.store(true, Ordering::SeqCst);
    LOADING.store(true, Ordering::SeqCst);
    {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        st.version = version.clone();
        st.date.clear();
        st.has_body = false;
        st.blocks.clear();
        st.toc.clear();
    }
    publish();

    crate::spawn(async move {
        let fetched = fetch_release_for_version(&version).await;
        apply(fetched);
    });
}

pub fn close() {
    OPEN.store(false, Ordering::SeqCst);
    // Dismissing is the acknowledgement, whichever card it happened on: the
    // deck has been seen for this version and will not open by itself again.
    // Reopening from the hamburger is always available and rewrites the same
    // value, so it never re-arms the automatic open.
    let version = crate::about_qt::app_version();
    if !deck_for(version).is_empty() {
        crate::settings_qt::save_pref(SEEN_PREF, serde_json::Value::String(version.to_string()));
    }
    publish();
}

/// Open the deck by itself, once per version. Called after the shell is up:
/// never over the login screen, and never when this version has no deck.
pub fn auto_open_if_new() {
    let version = crate::about_qt::app_version();
    if deck_for(version).is_empty() {
        return;
    }
    if crate::settings_qt::pref_str(SEEN_PREF, "") == version {
        return;
    }
    open();
}

/// Apply the fetched release (or its absence) to the document.
fn apply(fetched: Option<FetchedRelease>) {
    LOADING.store(false, Ordering::SeqCst);
    {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        match fetched {
            None => st.has_body = false,
            Some(rel) => {
                st.version = rel.version;
                st.date = rel.date;
                let (blocks, toc) = render_markdown(&rel.body.unwrap_or_default());
                if blocks.is_empty() {
                    st.has_body = false;
                } else {
                    st.blocks = blocks;
                    st.toc = toc;
                    st.has_body = true;
                }
            }
        }
    }
    publish();
}

/// Fetch the release for `version` by exact tag (`v{version}`), with the
/// GitHub-required `User-Agent`. `None` on any network/parse failure or for
/// draft/prerelease tags — which is why every DEV build shows the empty state.
async fn fetch_release_for_version(version: &str) -> Option<FetchedRelease> {
    let tag = if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    };
    let url = format!("{GITHUB_RELEASES_URL}/tags/{tag}");

    let builder = match qbz_net_proxy::apply_current(reqwest::Client::builder()) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("[qbz-qt] whats-new proxy config error: {e}");
            return None;
        }
    };
    let client = match builder
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("qbz")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[qbz-qt] whats-new client build failed: {e}");
            return None;
        }
    };

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[qbz-qt] whats-new fetch failed for {tag}: {e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        log::warn!("[qbz-qt] whats-new fetch HTTP {} for {tag}", resp.status());
        return None;
    }

    let release: GithubRelease = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[qbz-qt] whats-new JSON parse failed for {tag}: {e}");
            return None;
        }
    };
    if release.draft || release.prerelease {
        return None;
    }

    Some(FetchedRelease {
        version: normalize_version_tag(&release.tag_name),
        date: release_date_iso(&release.published_at),
        body: release.body,
    })
}

fn normalize_version_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('v').to_string()
}

/// Reduce GitHub's RFC3339 `published_at` to its civil date AS WRITTEN
/// (`YYYY-MM-DD`, offset ignored — the reference's chrono
/// `year()/month0()/day()` on a parsed `DateTime<FixedOffset>` did the same).
/// Display formatting happens in QML with `Qt.formatDate(..., Locale.LongFormat)`
/// so the header follows the user's locale instead of an English month table.
/// Malformed input is echoed verbatim.
fn release_date_iso(iso: &str) -> String {
    let bytes = iso.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return iso.to_string();
    }
    let num = |a: usize, b: usize| iso[a..b].parse::<u32>().ok();
    let (Some(_year), Some(month), Some(day)) = (num(0, 4), num(5, 7), num(8, 10)) else {
        return iso.to_string();
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return iso.to_string();
    }
    iso[..10].to_string()
}

// ==================== Markdown → blocks + TOC ====================
//
// Everything below is a transcription of whats_new.rs:201-412. The only
// changes are the Slint model types (`WhatsNewBlock`/`WhatsNewTocEntry`) giving
// way to the plain structs above, and `.into()` on the string fields becoming
// `.to_string()`. The LOGIC is byte-for-byte the reference's.

/// Strip inline `**bold**` / `` `code` `` markers and reduce inline markdown
/// links `[text](url)` to just their `text`.
fn strip_inline(text: &str) -> String {
    strip_markdown_links(text)
        .replace("**", "")
        .replace('`', "")
}

/// If `s[start..]` begins with a markdown link `[label](url)`, return
/// `(label, url, byte-index just past the ')')`. The `[](` `)` delimiters are
/// ASCII, so all returned slices sit on char boundaries. No nested brackets.
fn parse_link_at(s: &str, start: usize) -> Option<(&str, &str, usize)> {
    let rest = &s[start..];
    if !rest.starts_with('[') {
        return None;
    }
    let close_br = rest.find(']')?;
    if rest.as_bytes().get(close_br + 1) != Some(&b'(') {
        return None;
    }
    let open_paren = close_br + 1;
    let close_paren = open_paren + rest[open_paren..].find(')')?;
    let label = &rest[1..close_br];
    let url = &rest[open_paren + 1..close_paren];
    if url.is_empty() {
        return None;
    }
    Some((label, url, start + close_paren + 1))
}

/// Replace every inline `[text](url)` in a string with just its `text`.
fn strip_markdown_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == b'[' {
            if let Some((label, _url, end)) = parse_link_at(text, i) {
                out.push_str(label);
                i = end;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// If the whole trimmed line is exactly one markdown link, return `(text, url)`.
fn parse_standalone_link(s: &str) -> Option<(&str, &str)> {
    let t = s.trim();
    let (label, url, end) = parse_link_at(t, 0)?;
    (end == t.len()).then_some((label, url))
}

/// Slugify a heading label into a TOC anchor id (port of the Tauri `slugify`).
fn slugify(input: &str) -> String {
    let lowered = input.trim().to_lowercase();
    let mut cleaned = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if matches!(ch, '`' | '*' | '_' | '~') {
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-' {
            cleaned.push(ch);
        }
    }
    let mut out = String::with_capacity(cleaned.len());
    let mut last_hyphen = false;
    for ch in cleaned.chars() {
        if ch == ' ' || ch == '-' {
            if !last_hyphen {
                out.push('-');
                last_hyphen = true;
            }
        } else {
            out.push(ch);
            last_hyphen = false;
        }
    }
    out.trim_matches('-').to_string()
}

/// Count leading-space indentation (a tab counts as 2).
fn count_leading_spaces(line: &str) -> usize {
    let mut count = 0;
    for ch in line.chars() {
        match ch {
            ' ' => count += 1,
            '\t' => count += 2,
            _ => break,
        }
    }
    count
}

/// Push a section heading block; level-0 sections also become TOC entries.
fn push_heading(label: &str, level: i32, blocks: &mut Vec<Block>, toc: &mut Vec<TocEntry>) {
    let clean = label.trim();
    if clean.is_empty() {
        return;
    }
    let id = slugify(clean);
    if level == 0 {
        toc.push(TocEntry {
            id: id.clone(),
            label: clean.to_string(),
        });
    }
    blocks.push(Block {
        kind: KIND_SECTION,
        level,
        text: strip_inline(clean),
        id,
        url: String::new(),
    });
}

/// A clickable whole-line link block.
fn link_block(label: &str, url: &str) -> Block {
    Block {
        kind: KIND_LINK,
        level: 0,
        text: strip_inline(label),
        id: String::new(),
        url: url.to_string(),
    }
}

/// Render the release-notes markdown into a flat block model + a TOC of the
/// level-0 section headings. 1:1 with `whats_new.rs::render_markdown`.
pub fn render_markdown(markdown: &str) -> (Vec<Block>, Vec<TocEntry>) {
    let mut blocks: Vec<Block> = Vec::new();
    let mut toc: Vec<TocEntry> = Vec::new();

    if markdown.trim().is_empty() {
        return (blocks, toc);
    }

    for line in markdown.split('\n') {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Headings (#, ##, ###).
        if let Some(rest) = trimmed.strip_prefix("# ") {
            push_heading(rest, 0, &mut blocks, &mut toc);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            push_heading(rest, 0, &mut blocks, &mut toc);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            push_heading(rest, 1, &mut blocks, &mut toc);
            continue;
        }

        // List items with indentation-based nesting.
        let is_list = trimmed.starts_with("- ") || trimmed.starts_with("* ");
        if is_list {
            let indent = count_leading_spaces(line);
            let level = (indent / 2) as i32;
            let content = trimmed[2..].trim();

            if level == 0 {
                // Top-level bullets become section headings (no bullet glyph).
                push_heading(content, 0, &mut blocks, &mut toc);
                continue;
            }

            // A bullet that is nothing but a link renders as a clickable link.
            if let Some((label, url)) = parse_standalone_link(content) {
                blocks.push(link_block(label, url));
                continue;
            }

            blocks.push(Block {
                kind: KIND_BULLET,
                level,
                text: strip_inline(content),
                id: String::new(),
                url: String::new(),
            });
            continue;
        }

        // A paragraph that is nothing but a link renders as a clickable link.
        if let Some((label, url)) = parse_standalone_link(trimmed) {
            blocks.push(link_block(label, url));
            continue;
        }

        // Paragraph.
        blocks.push(Block {
            kind: KIND_PARAGRAPH,
            level: 0,
            text: strip_inline(trimmed),
            id: String::new(),
            url: String::new(),
        });
    }

    (blocks, toc)
}

// ---------------------------------------------------------------------------
// Tests
//
// The renderer is the ONE piece of this lane with real logic, and it CANNOT be
// eyeballed: `fetch_release_for_version` returns None for draft/prerelease
// tags, so any build off an unreleased version shows the empty state. These
// tests are the only thing standing between a "helpful" rewrite and a modal
// that quietly disagrees with the website off the same source.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A REAL shipped release body. TAG_DETAILS.md is what gets pasted into the
    /// GitHub release the app fetches, so this is the exact text the renderer
    /// sees in production.
    const REAL_BODY: &str = include_str!("../../../docs/release-2.0.2/TAG_DETAILS.md");

    #[test]
    fn h1_and_h2_collapse_to_the_same_level_and_both_enter_the_toc() {
        let (blocks, toc) = render_markdown("# Alpha\n## Beta\n### Gamma\n");
        assert_eq!(blocks.len(), 3);
        assert_eq!((blocks[0].kind, blocks[0].level), (KIND_SECTION, 0));
        assert_eq!((blocks[1].kind, blocks[1].level), (KIND_SECTION, 0));
        // ### is a SUB-section and does NOT enter the TOC.
        assert_eq!((blocks[2].kind, blocks[2].level), (KIND_SECTION, 1));
        assert_eq!(
            toc,
            vec![
                TocEntry {
                    id: "alpha".into(),
                    label: "Alpha".into()
                },
                TocEntry {
                    id: "beta".into(),
                    label: "Beta".into()
                },
            ]
        );
    }

    #[test]
    fn an_indent_zero_bullet_becomes_a_section_plus_a_toc_entry() {
        // THE rule a naive markdown renderer gets wrong.
        let (blocks, toc) = render_markdown("- Top level item\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, KIND_SECTION);
        assert_eq!(blocks[0].level, 0);
        assert_eq!(blocks[0].text, "Top level item");
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].id, "top-level-item");
    }

    #[test]
    fn bullet_level_is_leading_spaces_over_two_with_tabs_worth_two() {
        let (blocks, _) = render_markdown("  - one\n    - two\n\t- tab\n      * six\n");
        let levels: Vec<i32> = blocks.iter().map(|b| b.level).collect();
        assert_eq!(levels, vec![1, 2, 1, 3]);
        assert!(blocks.iter().all(|b| b.kind == KIND_BULLET));
        // `* ` is a bullet marker too.
        assert_eq!(blocks[3].text, "six");
    }

    #[test]
    fn inline_markers_are_stripped_not_styled() {
        let (blocks, _) = render_markdown("  - **Bold** and `code` and [label](https://x.dev)\n");
        assert_eq!(blocks[0].kind, KIND_BULLET);
        assert_eq!(blocks[0].text, "Bold and code and label");
        assert!(blocks[0].url.is_empty());
    }

    #[test]
    fn a_whole_line_link_becomes_a_clickable_link_block() {
        let (blocks, _) = render_markdown(
            "[Read the guide](https://github.com/vicrodh/qbz/wiki)\n  - [Nested](https://n.dev)\n",
        );
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, KIND_LINK);
        assert_eq!(blocks[0].text, "Read the guide");
        assert_eq!(blocks[0].url, "https://github.com/vicrodh/qbz/wiki");
        // A bullet whose whole content is a link is a link too, NOT a bullet.
        assert_eq!(blocks[1].kind, KIND_LINK);
        assert_eq!(blocks[1].url, "https://n.dev");
    }

    #[test]
    fn a_line_that_is_a_link_plus_trailing_text_stays_a_paragraph() {
        let (blocks, _) = render_markdown("[label](https://x.dev) and more\n");
        assert_eq!(blocks[0].kind, KIND_PARAGRAPH);
        assert_eq!(blocks[0].text, "label and more");
    }

    #[test]
    fn blank_lines_are_dropped_and_empty_input_renders_nothing() {
        let (blocks, toc) = render_markdown("\n\n   \n");
        assert!(blocks.is_empty());
        assert!(toc.is_empty());
        assert_eq!(render_markdown("").0.len(), 0);
    }

    #[test]
    fn slugify_collapses_runs_and_drops_punctuation() {
        assert_eq!(
            slugify("The headless daemon (qbzd)"),
            "the-headless-daemon-qbzd"
        );
        // Emphasis chars vanish; the comma and the `&` are dropped and the
        // whitespace run they leave behind collapses to ONE hyphen.
        assert_eq!(
            slugify("  **Library, home & discovery**  "),
            "library-home-discovery"
        );
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn release_dates_reduce_to_their_civil_date_and_fall_back_raw() {
        assert_eq!(release_date_iso("2026-07-04T18:22:01Z"), "2026-07-04");
        // The date AS WRITTEN, offset ignored (chrono's year()/month0()/day()
        // on a parsed DateTime<FixedOffset> behave the same).
        assert_eq!(release_date_iso("2026-12-31T00:00:00+02:00"), "2026-12-31");
        assert_eq!(release_date_iso("2026-01-09T00:00:00Z"), "2026-01-09");
        // Malformed input is echoed verbatim, never blanked.
        assert_eq!(release_date_iso("not a date"), "not a date");
        assert_eq!(release_date_iso("2026/07/04"), "2026/07/04");
        assert_eq!(release_date_iso("2026-13-04T00:00:00Z"), "2026-13-04T00:00:00Z");
        assert_eq!(release_date_iso(""), "");
    }

    #[test]
    fn version_tags_normalize() {
        assert_eq!(normalize_version_tag(" v2.0.2 "), "2.0.2");
        assert_eq!(normalize_version_tag("2.0.2"), "2.0.2");
    }

    #[test]
    fn the_real_2_0_2_release_body_renders_the_shape_the_modal_expects() {
        let (blocks, toc) = render_markdown(REAL_BODY);
        assert!(!blocks.is_empty());

        // The h1 title is the first block AND the first TOC entry.
        assert_eq!(blocks[0].kind, KIND_SECTION);
        assert_eq!(blocks[0].level, 0);
        assert_eq!(blocks[0].text, "2.0.2 — Rebuild 破 (You Can (Not) Advance)");
        assert_eq!(toc[0].label, blocks[0].text);

        // Every `##` heading is a level-0 section and a TOC entry.
        assert!(toc.iter().any(|e| e.label == "The headless daemon (qbzd)"));
        assert!(toc.len() > 3, "toc = {toc:?}");

        // The two-space bullets of TAG_DETAILS.md are level-1 bullets whose
        // `**lead-in**` has been stripped.
        let daemon_bullet = blocks
            .iter()
            .find(|b| b.text.starts_with("A player with no window"))
            .expect("the daemon bullet");
        assert_eq!(daemon_bullet.kind, KIND_BULLET);
        assert_eq!(daemon_bullet.level, 1);
        assert!(!daemon_bullet.text.contains("**"));

        // The standalone wiki link is a clickable block, not a paragraph.
        let wiki = blocks
            .iter()
            .find(|b| b.kind == KIND_LINK)
            .expect("a link block");
        assert!(wiki.url.starts_with("https://"), "{wiki:?}");

        // No block carries un-stripped inline markers.
        assert!(blocks
            .iter()
            .all(|b| !b.text.contains("**") && !b.text.contains('`')));
        // Nothing is a KIND_SECTION with a level above 1, and no bullet is
        // level 0 (that combination is what the indent-0 rule prevents).
        assert!(blocks.iter().all(|b| b.kind != KIND_BULLET || b.level >= 1));
        assert!(blocks
            .iter()
            .all(|b| b.kind != KIND_SECTION || b.level <= 1));
        // Paragraphs exist (the intro prose, and the `---` separators the
        // reference deliberately does not special-case).
        assert!(blocks.iter().any(|b| b.kind == KIND_PARAGRAPH));
    }
}
