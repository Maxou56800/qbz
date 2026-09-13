//! Performers string parsing utilities
//!
//! Qobuz provides performer credits as a formatted string like:
//! "John Coltrane, Saxophone, MainArtist - McCoy Tyner, Piano - Jimmy Garrison, Double Bass"
//!
//! This module parses that string into structured data.

use serde::{Deserialize, Serialize};

/// A performer with their name and roles
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Performer {
    pub name: String,
    pub roles: Vec<String>,
}

impl Performer {
    pub fn new(name: String, roles: Vec<String>) -> Self {
        Self { name, roles }
    }
}

/// Parse a Qobuz performers string into structured data
///
/// Format: "Name, Role1, Role2 - Name, Role1 - Name, Role1, Role2"
///
/// # Examples
///
/// ```
/// use qbz_qobuz::performers::parse_performers;
///
/// let performers = parse_performers("John Coltrane, Saxophone, MainArtist - McCoy Tyner, Piano");
/// assert_eq!(performers.len(), 2);
/// assert_eq!(performers[0].name, "John Coltrane");
/// assert_eq!(performers[0].roles, vec!["Saxophone", "MainArtist"]);
/// ```
pub fn parse_performers(performers_str: &str) -> Vec<Performer> {
    if performers_str.is_empty() {
        return Vec::new();
    }

    performers_str
        .split(" - ")
        .filter_map(|segment| {
            let segment = segment.trim();
            if segment.is_empty() {
                return None;
            }

            let parts: Vec<&str> = segment.split(", ").collect();
            if parts.is_empty() {
                return None;
            }

            let name = parts[0].trim().to_string();
            if name.is_empty() {
                return None;
            }

            let roles: Vec<String> = parts[1..]
                .iter()
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty())
                .collect();

            Some(Performer::new(name, roles))
        })
        .collect()
}

/// Group performers by their roles
///
/// Returns a map where keys are role names and values are lists of performer names
pub fn group_by_role(performers: &[Performer]) -> std::collections::HashMap<String, Vec<String>> {
    let mut grouped: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for performer in performers {
        for role in &performer.roles {
            grouped
                .entry(role.clone())
                .or_default()
                .push(performer.name.clone());
        }
    }

    grouped
}

/// Build the lookup key Qobuz frontends use for role i18n: first char
/// lowercased, the rest with spaces stripped (mirrors Tauri `formatRole`).
fn role_key(role: &str) -> String {
    let mut chars = role.chars();
    match chars.next() {
        Some(first) => {
            let lowered: String = first.to_lowercase().collect();
            let rest: String = chars.filter(|c| *c != ' ').collect();
            format!("{lowered}{rest}")
        }
        None => String::new(),
    }
}

/// Fallback humanizer (mirrors Tauri `formatUnknownRole` minus the final
/// upper-casing, which the caller applies): insert a space before each
/// uppercase letter, then trim. e.g. "CustomRole" -> "Custom Role".
fn humanize_role(role: &str) -> String {
    let mut out = String::with_capacity(role.len() + 4);
    for (i, c) in role.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// Human-readable role label, ported 1:1 from the Tauri `performerRoles` i18n
/// map (English) with the same `formatRole` key + `formatUnknownRole`
/// fallback. NOT upper-cased — the Track Info grid upper-cases at render
/// (Tauri uses CSS `text-transform: uppercase`).
pub fn format_role_label(role: &str) -> String {
    let key = role_key(role);
    for (k, label) in PERFORMER_ROLE_LABELS {
        if *k == key {
            return (*label).to_string();
        }
    }
    humanize_role(role)
}

/// Ordered, deduped role grouping — 1:1 with Tauri `getGroupedCredits`:
/// group performer names by role (dedup within a role, first-seen order),
/// then order roles as: Composer, Lyricist first (Composer before Lyricist);
/// MainArtist / "Main Artist" last; everything else alphabetical
/// (case-insensitive). Returns `(role, names)` pairs.
pub fn group_credits_ordered(performers: &[Performer]) -> Vec<(String, Vec<String>)> {
    // Preserve first-seen role order while grouping (mirror of JS object key
    // insertion order before the explicit sort).
    let mut order: Vec<String> = Vec::new();
    let mut grouped: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for performer in performers {
        for role in &performer.roles {
            let entry = grouped.entry(role.clone()).or_insert_with(|| {
                order.push(role.clone());
                Vec::new()
            });
            if !entry.contains(&performer.name) {
                entry.push(performer.name.clone());
            }
        }
    }

    let is_first = |r: &str| {
        let r = r.to_lowercase();
        r == "composer" || r == "lyricist"
    };
    let is_last = |r: &str| {
        let r = r.to_lowercase();
        r == "mainartist" || r == "main artist"
    };

    order.sort_by(|a, b| {
        use std::cmp::Ordering;
        let (af, bf) = (is_first(a), is_first(b));
        let (al, bl) = (is_last(a), is_last(b));
        if af && !bf {
            return Ordering::Less;
        }
        if !af && bf {
            return Ordering::Greater;
        }
        if al && !bl {
            return Ordering::Greater;
        }
        if !al && bl {
            return Ordering::Less;
        }
        if af && bf {
            if a.to_lowercase() == "composer" {
                return Ordering::Less;
            }
            if b.to_lowercase() == "composer" {
                return Ordering::Greater;
            }
        }
        a.to_lowercase().cmp(&b.to_lowercase())
    });

    order
        .into_iter()
        .map(|role| {
            let names = grouped.remove(&role).unwrap_or_default();
            (role, names)
        })
        .collect()
}

/// Names to append as "feat. …" after a row's artist line: every performer
/// credited `FeaturedArtist` or `MainArtist` in `performers`, in string
/// order, deduplicated, minus anyone the row already names (`row_artist` and
/// its `&` / `,` / ` and ` / ` x ` / ` feat. ` parts) and minus anyone the
/// title already credits ("Song (feat. X)"). Empty when there is nothing to
/// add. Pure; the row producers join the result with ", ".
pub fn featured_artists(performers: &str, row_artist: &str, title: &str) -> Vec<String> {
    let already: Vec<String> = split_credit_names(row_artist).map(normalize_name).collect();
    let title_norm = normalize_name(title);
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for p in parse_performers(performers) {
        let credited = p.roles.iter().any(|r| {
            let k = role_key(r);
            k == "featuredArtist" || k == "mainArtist"
        });
        if !credited {
            continue;
        }
        let key = normalize_name(&p.name);
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.push(key.clone());
        if already.iter().any(|a| a == &key) {
            continue;
        }
        if !title_norm.is_empty() && title_norm.contains(&key) {
            continue;
        }
        out.push(p.name);
    }
    out
}

/// "Is this the same person" for two credited names (the row's featured
/// performer vs. an album credit that carries an id).
pub fn same_artist_name(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_name(a), normalize_name(b));
    !a.is_empty() && a == b
}

/// Lower-case, Latin-1 diacritics folded, apostrophes dropped, whitespace
/// collapsed — the comparison key for "is this the same person".
fn normalize_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'ý' | 'ÿ' => 'y',
            '\'' | '’' | '`' => continue,
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if c == ' ' && out.ends_with(' ') {
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// The individual names a row's artist string spells out ("A & B", "A, B",
/// "A and B", "A x B", "A feat. B").
fn split_credit_names(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| c == '&' || c == ',' || c == '/' || c == '+')
        .flat_map(|part| part.split(" and "))
        .flat_map(|part| part.split(" x "))
        .flat_map(|part| part.split(" feat. "))
        .flat_map(|part| part.split(" ft. "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_performer() {
        let result = parse_performers("John Coltrane, Saxophone");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "John Coltrane");
        assert_eq!(result[0].roles, vec!["Saxophone"]);
    }

    #[test]
    fn test_parse_multiple_performers() {
        let result = parse_performers(
            "John Coltrane, Saxophone, MainArtist - McCoy Tyner, Piano - Elvin Jones, Drums",
        );
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "John Coltrane");
        assert_eq!(result[0].roles, vec!["Saxophone", "MainArtist"]);
        assert_eq!(result[1].name, "McCoy Tyner");
        assert_eq!(result[1].roles, vec!["Piano"]);
        assert_eq!(result[2].name, "Elvin Jones");
        assert_eq!(result[2].roles, vec!["Drums"]);
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_performers("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_performer_no_roles() {
        let result = parse_performers("John Coltrane");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "John Coltrane");
        assert!(result[0].roles.is_empty());
    }

    #[test]
    fn test_group_by_role() {
        let performers = vec![
            Performer::new("John".to_string(), vec!["Saxophone".to_string()]),
            Performer::new(
                "Jane".to_string(),
                vec!["Saxophone".to_string(), "Vocals".to_string()],
            ),
        ];
        let grouped = group_by_role(&performers);
        assert_eq!(grouped.get("Saxophone").unwrap().len(), 2);
        assert_eq!(grouped.get("Vocals").unwrap().len(), 1);
    }

    const NOTHING_ELSE_MATTERS: &str = "Watt, FeaturedArtist - Robert Trujillo, Bass, FeaturedArtist - Elton John, Piano, FeaturedArtist - ALAN MOULDER, MixingEngineer - Paul Lamalfa, Engineer - Yo-Yo Ma, Cello, FeaturedArtist - Miley Cyrus, Vocalist, MainArtist - Chad Smith, FeaturedArtist, DrumKit - James Hetfield, ComposerLyricist - Lars Ulrich, ComposerLyricist - Mike Block, StringArranger - Andrew Watt, Producer, Guitar, StringArranger - Caesar Edmunds, MixingSecondEngineer - Randy Merrill, MasteringEngineer";

    /// The real search sample: MainArtist mid-string, featured artists with
    /// instruments, crew roles to ignore.
    #[test]
    fn featured_artists_keeps_featured_and_extra_mains_in_string_order_minus_the_row_artist() {
        assert_eq!(
            featured_artists(NOTHING_ELSE_MATTERS, "Miley Cyrus", "Nothing Else Matters"),
            vec!["Watt", "Robert Trujillo", "Elton John", "Yo-Yo Ma", "Chad Smith"]
        );
    }

    #[test]
    fn featured_artists_skips_names_the_row_or_the_title_already_credits() {
        let p = "Alice, MainArtist - Bob, MainArtist - Carol, FeaturedArtist";
        assert_eq!(featured_artists(p, "Alice & Bob", "Song"), vec!["Carol"]);
        assert_eq!(featured_artists(p, "Alice", "Song (feat. Carol)"), vec!["Bob"]);
        assert!(featured_artists(p, "Alice, Bob & Carol", "Song").is_empty());
    }

    #[test]
    fn same_artist_name_folds_case_diacritics_and_apostrophes() {
        assert!(same_artist_name("Émilie D'Angelo", "emilie dangelo"));
        assert!(same_artist_name("Yo-Yo Ma", "YO-YO  MA"));
        assert!(!same_artist_name("Watt", "Andrew Watt"));
        assert!(!same_artist_name("", ""));
    }

    #[test]
    fn featured_artists_dedupes_case_and_diacritics_and_ignores_crew() {
        let p = "Émilie, FeaturedArtist - emilie, Vocalist, FeaturedArtist - Dan, Producer - Main, MainArtist";
        assert_eq!(featured_artists(p, "Main", ""), vec!["Émilie"]);
        assert!(featured_artists("", "Main", "").is_empty());
        assert!(featured_artists("Solo, MainArtist", "Solo", "Song").is_empty());
    }

}
pub(crate) static PERFORMER_ROLE_LABELS: &[(&str, &str)] = &[
    ("a&R", "Artists and Repertoire"),
    ("a&RDirector", "Artists and Repertoire Director"),
    ("aAndRCoordinator", "A&R Coordinator"),
    ("accordion", "Accordion"),
    ("acousticGuitar", "Acoustic Guitar"),
    ("additionalEngineer", "Additional Engineer"),
    ("additionalKeyboard", "Additional Keyboard"),
    ("additionalMusic", "Additional Music"),
    ("additionalProduction", "Additional Production"),
    ("additionalProgrammer", "Additional Programmer"),
    ("additionalStudioProducer", "Additional Studio Producer"),
    ("additionalVocalist", "Additional Vocalist"),
    ("additionalVocals", "Additional Vocals"),
    ("arranger", "Arranger"),
    ("artDirector", "Art Director"),
    ("artist", "Artist"),
    ("assistant", "Assistant"),
    ("assistantEngineer", "Assistant Engineer"),
    ("assistantMixer", "Assistant Mixer"),
    ("assistantMixingEngineer", "Assistant Mixing Engineer"),
    ("associatedPerformer", "Associated Performer"),
    ("associatedProducer", "Associated Producer"),
    ("author", "Author"),
    ("backingVocals", "Backing Vocals"),
    ("backgroundVocal", "Background Vocal"),
    ("backgroundVocalist", "Background Vocalist"),
    ("backgroundVocals", "Background Vocals"),
    ("baritoneSaxophone", "Baritone Saxophone"),
    ("bass", "Bass"),
    ("bassGuitar", "Bass Guitar"),
    ("celesta", "Celesta"),
    ("cello", "Cello"),
    ("chamberOrchestra", "Chamber Orchestra"),
    ("choir", "Choir"),
    ("choirArranger", "Choir Arranger"),
    ("choirConductor", "Choir Conductor"),
    ("coachVocals", "Coach Vocals"),
    ("composer", "Composer"),
    ("composerLyricist", "Composer Lyricist"),
    ("conductor", "Conductor"),
    ("consultant", "Consultant"),
    ("contractor", "Contractor"),
    ("coProducer", "Co-Producer"),
    ("designer", "Designer"),
    ("digitalEditingEngineer", "Digital Editing Engineer"),
    ("doubleBass", "Double Bass"),
    ("drum", "Drum"),
    ("drumMachine", "Drum Machine"),
    ("drumProgrammer", "Drum Programmer"),
    ("drumProgramming", "Drum Programming"),
    ("drumKit", "Drum Kit"),
    ("drums", "Drums"),
    ("editingEngineer", "Editing Engineer"),
    ("electricBassGuitar", "Electric Bass Guitar"),
    ("electricGuitar", "Electric Guitar"),
    ("engineer", "Engineer"),
    ("executiveProducer", "Executive Producer"),
    ("featuredArtist", "Featured Artist"),
    ("fiddle", "Fiddle"),
    ("frenchHorn", "French Horn"),
    ("glockenspiel", "Glockenspiel"),
    ("guitar", "Guitar"),
    ("hammondOrgan", "Hammond Organ"),
    ("harp", "Harp"),
    ("horn", "Horn"),
    ("horns", "Horns"),
    ("instrumentation", "Instrumentation"),
    ("keyboard", "Keyboard"),
    ("keyboards", "Keyboards"),
    ("leadGuitar", "Lead Guitar"),
    ("leadViolin", "Lead Violin"),
    ("leadVocals", "Lead Vocals"),
    ("lyricist", "Lyricist"),
    ("mainArtist", "Main Artist"),
    ("manager", "Manager"),
    ("masterer", "Masterer"),
    ("masteringEngineer", "Mastering Engineer"),
    ("mixer", "Mixer"),
    ("mixEngineer", "Mix Engineer"),
    ("mixingEngineer", "Mixing Engineer"),
    ("mixingSecondEngineer", "Mixing Second Engineer"),
    ("music", "Music"),
    ("musicDirector", "Music Director"),
    ("musicEditor", "Music Editor"),
    ("musicPublisher", "Music Publisher"),
    ("musicSupervisor", "Music Supervisor"),
    ("nylonStringGuitar", "Nylon String Guitar"),
    ("orchestra", "Orchestra"),
    ("orchestralContractor", "Orchestral Contractor"),
    ("orchestration", "Orchestration"),
    ("organ", "Organ"),
    ("other", "Other"),
    ("oud", "Oud"),
    ("percussion", "Percussion"),
    ("performance", "Performance"),
    ("performer", "Performer"),
    ("piano", "Piano"),
    ("producer", "Producer"),
    ("production", "Production"),
    ("programmer", "Programmer"),
    ("programming", "Programming"),
    ("programmingEngineer", "Programming Engineer"),
    ("rapVocalist", "Rap Vocalist"),
    ("recordedby", "Recorded By"),
    ("recordingArranger", "Recording Arranger"),
    ("recordingEngineer", "Recording Engineer"),
    ("recordingProducer", "Recording Producer"),
    ("recordingSecondEngineer", "Recording Second Engineer"),
    ("remasteringEngineer", "Remastering Engineer"),
    ("remixer", "Remixer"),
    ("remixingSecondEngineer", "Remixing Second Engineer"),
    ("rhythmGuitar", "rhythmGuitar"),
    ("samples", "Samples"),
    ("saxophone", "Saxophone"),
    ("secondEngineer", "Second Engineer"),
    ("singer", "Singer"),
    ("slideguitar&Pad", "Slide Guitar & Pad"),
    ("songwriter", "Songwriter"),
    ("stringArranger", "String Arranger"),
    ("strings", "Strings"),
    ("studioAssistant", "Studio Assistant"),
    ("studioPersonnel", "Studio Personnel"),
    ("synthesizer", "Synthesizer"),
    ("synthesizerProgrammer", "Synthesizer Programmer"),
    ("synthesizerProgramming", "Synthesizer Programming"),
    ("technicalAssistant", "Technical Assistant"),
    ("technicalEngineer", "Technical Engineer"),
    ("technician", "Technician"),
    ("trombone", "Trombone"),
    ("trumpet", "Trumpet"),
    ("tuba", "Tuba"),
    ("ukulele", "Ukulele"),
    ("unknown", "Unknown"),
    ("vibraphone", "Vibraphone"),
    ("viola", "Viola"),
    ("violin", "Violin"),
    ("vocal", "Vocal"),
    ("vocalArranger", "Vocal Arranger"),
    ("vocalEditingEngineer", "Vocal Editing Engineer"),
    ("vocalEngineer", "Vocal Engineer"),
    ("vocalProducer", "Vocal Producer"),
    ("vocalist", "Vocalist"),
    ("vocals", "Vocals"),
    ("vocals&Guitar", "Vocals & Guitar"),
    ("voice", "Voice"),
    ("workArranger", "Work Arranger"),
    ("writer", "Writer"),
];
