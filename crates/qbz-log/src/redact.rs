//! Write-time secret redaction — the single most important safety layer in this crate.
//!
//! Two layers, applied in this order by [`redact`]:
//!   1. **Literal live-secret layer:** exact-string replacement of values registered via
//!      [`register_secret`] right after login (the real `user_auth_token`, `app_secret`, …).
//!      Catches tokens logged without a labeled key, which the regexes can't anticipate.
//!   2. **Regex layer:** a fixed set of labeled-key patterns (auth tokens, request_sig,
//!      app_secret, password, bearer/authorization, access/refresh tokens, URL `token=`).
//!      A cheap `.contains` pre-check short-circuits lines with no candidate substring.
//!
//! Every match collapses the secret VALUE to `***REDACTED***` while preserving the
//! labeled key prefix (capture group 1) so the line stays debuggable.

use std::sync::{OnceLock, RwLock};

use regex::Regex;

const REPLACEMENT: &str = "***REDACTED***";
/// Live secret values shorter than this are ignored (too generic to scrub safely).
const MIN_SECRET_LEN: usize = 6;

/// Compiled redaction patterns. Group 1 captures the labeled-key prefix that is kept;
/// the trailing value is what gets replaced. Patterns are case-insensitive.
fn patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Qobuz user auth token (labeled key, JSON/query/assignment forms)
            r#"(?i)(user_auth_token["':=\s]+)[A-Za-z0-9._\-]+"#,
            // …and the request header form
            r#"(?i)(x-user-auth-token:\s*)\S+"#,
            // request_sig MD5 hex (labeled key form, hex >= 8)
            r#"(?i)(request_sig["':=\s]+)[a-f0-9]{8,}"#,
            // request_sig as a bare URL query param
            r#"(?i)(request_sig=)[a-f0-9]+"#,
            // app secret (app_secret / appsecret)
            r#"(?i)(app_?secret["':=\s]+)[A-Za-z0-9]+"#,
            // password
            r#"(?i)(password["':=\s]+)[^\s"',&]+"#,
            // authorization: Bearer <token>
            r#"(?i)(authorization:\s*bearer\s+)\S+"#,
            // bare bearer token
            r#"(?i)(bearer\s+)[A-Za-z0-9._\-]+"#,
            // OAuth access/refresh tokens (labeled key form)
            r#"(?i)((access|refresh)_token["':=\s]+)[A-Za-z0-9._\-]+"#,
            // generic URL token param
            r#"(?i)(token=)[^&\s"']+"#,
            // Signed CDN URLs: Qobuz/Akamai `hmac=`, CloudFront `Signature=` /
            // `Policy=` / `Key-Pair-Id=`. Value = up to the next query separator.
            r#"(?i)(hmac=)[^&\s"']+"#,
            r#"(?i)(signature=)[^&\s"']+"#,
            r#"(?i)(policy=)[^&\s"']+"#,
            r#"(?i)(key-pair-id=)[^&\s"']+"#,
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    })
}

fn secrets() -> &'static RwLock<Vec<String>> {
    static SECRETS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register a live secret value so the literal layer scrubs it everywhere, even when
/// logged without a labeled key. Empty / very short values (< [`MIN_SECRET_LEN`]) are
/// ignored, and duplicates are skipped.
pub fn register_secret(value: String) {
    if value.len() < MIN_SECRET_LEN {
        return;
    }
    if let Ok(mut guard) = secrets().write() {
        if !guard.iter().any(|s| s == &value) {
            guard.push(value);
        }
    }
}

/// Cheap pre-check: does the line contain any substring that one of the regexes could
/// match? Avoids running the whole pattern set on the overwhelming majority of lines.
fn has_redaction_candidate(lower: &str) -> bool {
    const NEEDLES: [&str; 9] = [
        "token", "secret", "password", "bearer", "auth", "sig", "hmac", "policy", "key-pair",
    ];
    NEEDLES.iter().any(|n| lower.contains(n))
}

/// IPv4 octet (0-255) — the range guard is what keeps version strings
/// ("1.16.1", "8.2.0") and timestamps out of the mask.
const OCTET: &str = r"(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])";

/// IP-masking patterns (owner 2026-07-24): ANY address — LAN or public — is
/// masked to its LAST segment only (`192.168.178.211` → `*.*.*.211`), so
/// uploaded logs stop leaking the user's network topology. The port survives.
///
/// IPv6 groups must START with a 4-hex-digit group — that is what keeps time
/// strings ("11:41:48.054") and MAC addresses out of the mask (regex has no
/// lookaround, so the width anchor does the disambiguation).
fn ip_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // IPv4: mask the first three octets, keep the last (capture 1).
            format!(r"\b(?:{OCTET}\.){{3}}({OCTET})\b"),
            // IPv6 full form (first group 4 hex wide — see the doc above).
            r"\b[0-9a-fA-F]{4}:(?:[0-9a-fA-F]{1,4}:){1,6}([0-9a-fA-F]{1,4})\b".to_string(),
            // IPv6 `::`-compressed form (same 4-wide anchor).
            r"\b[0-9a-fA-F]{4}(?::[0-9a-fA-F]{0,4})*::(?:[0-9a-fA-F]{1,4}:)*([0-9a-fA-F]{1,4})\b".to_string(),
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    })
}

/// Cheap pre-check for the IP layer: a dot or colon and a digit, O(n). (The
/// colon is required — IPv6 has no dots.)
fn has_ip_candidate(line: &str) -> bool {
    (line.contains('.') || line.contains(':')) && line.bytes().any(|b| b.is_ascii_digit())
}

/// Redact secrets from a single log line. Literal live-secret layer first, then regex.
pub fn redact(line: &str) -> String {
    let mut out = line.to_string();

    // Layer 1 — literal live secrets.
    if let Ok(guard) = secrets().read() {
        for secret in guard.iter() {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REPLACEMENT);
            }
        }
    }

    // Layer 2 — labeled-key regexes (guarded by a cheap substring pre-check).
    let lower = out.to_ascii_lowercase();
    if has_redaction_candidate(&lower) {
        for re in patterns() {
            if re.is_match(&out) {
                out = re
                    .replace_all(&out, format!("${{1}}{REPLACEMENT}").as_str())
                    .into_owned();
            }
        }
    }

    // Layer 3 — IP masking (`*.*.*.<last>` for v4, `*:*:<last>` for v6).
    if has_ip_candidate(&out) {
        for (i, re) in ip_patterns().iter().enumerate() {
            if re.is_match(&out) {
                let replacement = if i == 0 { "*.*.*.${1}" } else { "*:*:${1}" };
                out = re.replace_all(&out, replacement).into_owned();
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_all_known_shapes() {
        for s in [
            "GET /track?request_sig=ab12cd34ef56 user_auth_token=SEKRET_TOKEN_123",
            "X-User-Auth-Token: SEKRET_TOKEN_123",
            r#"{"app_secret":"abc123def","password":"hunter2"}"#,
            "authorization: Bearer eyJ.aaa.bbb",
        ] {
            let r = redact(s);
            assert!(!r.contains("SEKRET_TOKEN_123"), "leaked auth token: {r}");
            assert!(!r.contains("ab12cd34ef56"), "leaked request_sig: {r}");
            assert!(!r.contains("abc123def"), "leaked app_secret: {r}");
            assert!(!r.contains("hunter2"), "leaked password: {r}");
            assert!(!r.contains("eyJ.aaa.bbb"), "leaked bearer token: {r}");
        }
    }

    #[test]
    fn redacts_signed_cdn_url_params() {
        let r = redact(
            "fetch: https://streaming.example/file?eid=1&hmac=HMACSECRET&etsp=1700000000 \
             and https://cf.example/l.json?Policy=POLICYBLOB&Signature=SIGBLOB&Key-Pair-Id=KPID123",
        );
        for leaked in ["HMACSECRET", "POLICYBLOB", "SIGBLOB", "KPID123"] {
            assert!(!r.contains(leaked), "leaked {leaked}: {r}");
        }
        assert!(r.contains("etsp=1700000000"), "non-secret params survive: {r}");
        assert!(r.contains("eid=1"), "non-secret params survive: {r}");
    }

    #[test]
    fn literal_registry_scrubs_unlabeled_value() {
        register_secret("LIVE_TOKEN_xyz".into());
        let r = redact("blah LIVE_TOKEN_xyz blah");
        assert!(!r.contains("LIVE_TOKEN_xyz"), "literal secret survived: {r}");
        assert!(r.contains(REPLACEMENT), "no redaction marker: {r}");
    }

    #[test]
    fn short_secret_is_ignored() {
        register_secret("abc".into()); // < MIN_SECRET_LEN -> not registered
        let r = redact("value abc here");
        assert!(r.contains("abc"), "short value should not be scrubbed: {r}");
    }

    #[test]
    fn masks_ipv4_keep_last_octet() {
        let r = redact("DLNA: Set URI to http://192.168.178.211:9876/audio/***/235314822");
        assert_eq!(
            r,
            "DLNA: Set URI to http://*.*.*.211:9876/audio/***/235314822"
        );
        // Public addresses get the same treatment.
        let r = redact("dial 203.0.113.9 failed");
        assert!(r.contains("*.*.*.9"), "public IPv4 survived: {r}");
        assert!(!r.contains("203.0.113"), "public IPv4 leaked: {r}");
    }

    #[test]
    fn ip_mask_leaves_versions_and_timestamps_alone() {
        for s in [
            "2026-07-24 11:41:48.054 INFO qbz",
            "bundle 8.2.0-b034",
            "slint 1.16.1",
            "kernel 6.17.0-40-generic",
            "rate 44.1kHz 24-bit / 192 kHz",
        ] {
            assert_eq!(redact(s), s, "non-IP was mangled: {s}");
        }
    }

    #[test]
    fn masks_ipv6() {
        let r = redact("bind fe80:0000:0000:0000:0224:a4ff:fe12:3456 up");
        assert!(r.contains("*:*:3456"), "full IPv6 survived: {r}");
        let r2 = redact("bind fe80::224:a4ff:fe12:3456 up");
        assert!(r2.contains("*:*:3456"), "compressed IPv6 survived: {r2}");
    }
}
