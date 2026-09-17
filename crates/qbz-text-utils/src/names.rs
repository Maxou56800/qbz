//! Placeholder names the catalog uses where a real credit is missing.
//!
//! Qobuz ships literal fillers as artist / composer / performer names —
//! "Not Documented" is the one seen on album headers (2026-09-13), and the
//! usual "Unknown" / "N/A" family exists too. They are not people: a UI
//! that shows them prints junk beside the real artist and links to an
//! artist page that does not exist. Every credit list runs through
//! [`is_placeholder_name`] before it reaches a document.

/// True for a name the catalog uses as "no data": matched case-insensitively
/// on the trimmed text, punctuation-insensitive for the hyphenated forms.
pub fn is_placeholder_name(name: &str) -> bool {
    let folded: String = name
        .trim()
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c.to_ascii_lowercase() })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    matches!(
        folded.as_str(),
        "" | "not documented"
            | "undocumented"
            | "unknown"
            | "unknown artist"
            | "unknown composer"
            | "n/a"
            | "na"
            | "none"
            | "null"
            | "no artist"
            | "no name"
            | "[unknown]"
            | "(unknown)"
    )
}

#[cfg(test)]
mod tests {
    use super::is_placeholder_name;

    #[test]
    fn the_catalog_fillers_are_placeholders() {
        for name in [
            "Not Documented",
            "not documented",
            "NOT-DOCUMENTED",
            "  Undocumented ",
            "Unknown",
            "Unknown Artist",
            "N/A",
            "",
            "   ",
        ] {
            assert!(is_placeholder_name(name), "{name:?}");
        }
    }

    #[test]
    fn real_names_pass() {
        for name in ["Stream Of Passion", "Various Artists", "Roy Orbison", "Non Documenté"] {
            assert!(!is_placeholder_name(name), "{name:?}");
        }
    }
}
