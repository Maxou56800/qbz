//! Shared HTTP client for all providers.
//!
//! The Tauri original built a new connection pool per request (`reqwest::get`
//! in four call sites, `Client::new()` per Tidal fetch, plus a builder client
//! per token fetch) — each one its own fd pool, which exhausted file
//! descriptors (EMFILE) on large imports. One `OnceLock` client fixes that;
//! async flavor of the existing blocking precedent in
//! `qbz-media-controls/src/notify.rs`.
//!
//! UA policy (behavior-faithful): NO default User-Agent on the client —
//! the scrapers go out exactly as before; only the Tidal proxy token request
//! sets [`USER_AGENT`] per-request, as the original did.

/// User-Agent sent to the QBZ credential proxy (Tidal token fetch only).
pub(crate) const USER_AGENT: &str = "QBZ/1.0.0";

const CONNECT_TIMEOUT_SECS: u64 = 10;

pub(crate) fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        qbz_net_proxy::apply_current(reqwest::Client::builder())
            .expect("apply proxy configuration")
            .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .build()
            .expect("static reqwest client")
    })
}
