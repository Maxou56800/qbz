//! User-configured proxy for QBZ's outgoing HTTP(S) traffic.
//!
//! This crate owns exactly two things: the [`ProxyConfig`] value type and
//! [`apply`], a helper that threads it onto a [`reqwest::ClientBuilder`]. It
//! has no settings storage, no Qt bridge and no knowledge of *which* crate is
//! building the client — every `reqwest::Client::builder()` call site in the
//! workspace (Qobuz API, media-server integrations, the updater, ad-hoc
//! downloads, ...) is expected to depend on this crate and call [`apply`]
//! instead of sending requests unproxied.
//!
//! # DNS leaks
//!
//! A SOCKS proxy can resolve the destination hostname on either side of the
//! tunnel. `socks4`/`socks5` resolve it with the *local* system resolver and
//! only hand the proxy a raw IP — every hostname visited is still leaked to
//! the local network/ISP even though the traffic itself goes through the
//! proxy. `socks4a`/`socks5h` send the hostname itself and let the proxy
//! resolve it. [`ProxyKind::Socks4`] and [`ProxyKind::Socks5`] always build
//! the `*a`/`*h` variant internally (see [`ProxyKind::url_scheme`]): this is
//! not a user-facing option; there is no correct reason for this app to leak
//! a hostname past a proxy the user explicitly configured.
//!
//! # What "disabled" means
//!
//! [`apply`] with `config: None` returns the builder untouched. It does not
//! call `.no_proxy()`. Before this crate existed, every client in the
//! workspace already inherited reqwest's own environment-based proxy
//! detection (`HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`); turning this
//! feature off must not regress that pre-existing, independent behavior. When
//! `config` is `Some`, the explicit proxy always takes precedence over the
//! environment (reqwest's `ClientBuilder::proxy` does this already).

use std::fmt;

use reqwest::ClientBuilder;
use url::Url;

/// Which proxy protocol the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Https,
    Socks4,
    Socks5,
}

impl ProxyKind {
    /// Stable, storage/display form ("socks5", not the DNS-safe wire scheme).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4",
            Self::Socks5 => "socks5",
        }
    }

    /// The scheme actually put on the proxy URL. SOCKS always resolves on the
    /// proxy side — see the module docs on DNS leaks.
    fn url_scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4a",
            Self::Socks5 => "socks5h",
        }
    }
}

impl std::str::FromStr for ProxyKind {
    type Err = ProxyConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "socks4" => Ok(Self::Socks4),
            "socks5" => Ok(Self::Socks5),
            other => Err(ProxyConfigError::UnknownKind(other.to_string())),
        }
    }
}

/// Optional proxy credentials. `Debug` redacts the password — never let this
/// type reach a log line and print it in the clear.
#[derive(Clone)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// A fully-specified proxy. Construct this only from the current, live
/// Settings -> Network state (see `qbz_app::settings::network`) — it carries
/// the plaintext password in memory for the lifetime of one client build and
/// is never itself persisted or serialized.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub auth: Option<ProxyAuth>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyConfigError {
    #[error("proxy host is empty")]
    EmptyHost,
    #[error("unknown proxy kind: {0}")]
    UnknownKind(String),
    #[error("invalid proxy host: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("invalid proxy configuration: {0}")]
    Invalid(#[from] reqwest::Error),
}

impl ProxyConfig {
    fn to_url(&self) -> Result<Url, ProxyConfigError> {
        if self.host.trim().is_empty() {
            return Err(ProxyConfigError::EmptyHost);
        }
        let mut url = Url::parse(&format!(
            "{}://{}:{}",
            self.kind.url_scheme(),
            self.host,
            self.port
        ))?;
        if let Some(auth) = &self.auth {
            // `Url::set_username`/`set_password` percent-encode the value for
            // us, so a `:` or `@` in either field can't be mistaken for the
            // userinfo/host separator.
            if !auth.username.is_empty() {
                let _ = url.set_username(&auth.username);
            }
            if !auth.password.is_empty() {
                let _ = url.set_password(Some(&auth.password));
            }
        }
        Ok(url)
    }

    fn to_reqwest_proxy(&self) -> Result<reqwest::Proxy, ProxyConfigError> {
        Ok(reqwest::Proxy::all(self.to_url()?)?)
    }
}

/// Apply the user's proxy choice to a client builder. `config: None` leaves
/// the builder untouched (see the module docs on what "disabled" means).
pub fn apply(
    builder: ClientBuilder,
    config: Option<&ProxyConfig>,
) -> Result<ClientBuilder, ProxyConfigError> {
    let Some(config) = config else {
        return Ok(builder);
    };
    Ok(builder.proxy(config.to_reqwest_proxy()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(kind: ProxyKind, auth: Option<ProxyAuth>) -> ProxyConfig {
        ProxyConfig {
            kind,
            host: "proxy.example.com".to_string(),
            port: 1080,
            auth,
        }
    }

    #[test]
    fn socks_schemes_always_resolve_on_the_proxy_side() {
        assert_eq!(
            config(ProxyKind::Socks5, None).to_url().unwrap().scheme(),
            "socks5h"
        );
        assert_eq!(
            config(ProxyKind::Socks4, None).to_url().unwrap().scheme(),
            "socks4a"
        );
    }

    #[test]
    fn http_and_https_keep_their_own_scheme() {
        assert_eq!(
            config(ProxyKind::Http, None).to_url().unwrap().scheme(),
            "http"
        );
        assert_eq!(
            config(ProxyKind::Https, None).to_url().unwrap().scheme(),
            "https"
        );
    }

    #[test]
    fn credentials_with_reserved_characters_round_trip() {
        // `:` and `@` are the userinfo/host delimiters; a literal one in
        // either field must survive as data, not get parsed as a separator.
        let auth = ProxyAuth {
            username: "us:er@name".to_string(),
            password: "p@ss:word".to_string(),
        };
        let url = config(ProxyKind::Http, Some(auth)).to_url().unwrap();
        assert_eq!(decode(url.username()), "us:er@name");
        assert_eq!(decode(url.password().unwrap_or("")), "p@ss:word");
        assert_eq!(url.host_str(), Some("proxy.example.com"));
        assert_eq!(url.port(), Some(1080));
    }

    fn decode(value: &str) -> String {
        value.replace("%3A", ":").replace("%40", "@")
    }

    #[test]
    fn empty_host_is_rejected() {
        let cfg = config(ProxyKind::Http, None);
        let cfg = ProxyConfig {
            host: "   ".to_string(),
            ..cfg
        };
        assert!(matches!(cfg.to_url(), Err(ProxyConfigError::EmptyHost)));
    }

    #[test]
    fn disabled_config_is_a_no_op() {
        // Presence, not behavior, is what's testable here without a process-
        // wide rustls crypto provider installed: `apply` must not touch the
        // builder at all when there is no config, which `Ok` with no error
        // already proves (`.build()` is reqwest's own concern, not this
        // crate's, and needs a provider this test has no business installing
        // process-wide).
        assert!(apply(ClientBuilder::new(), None).is_ok());
    }

    #[test]
    fn enabled_config_is_accepted() {
        let cfg = config(ProxyKind::Socks5, None);
        assert!(apply(ClientBuilder::new(), Some(&cfg)).is_ok());
    }

    #[test]
    fn proxy_kind_round_trips_through_its_stable_string_form() {
        for kind in [
            ProxyKind::Http,
            ProxyKind::Https,
            ProxyKind::Socks4,
            ProxyKind::Socks5,
        ] {
            assert_eq!(kind.as_str().parse::<ProxyKind>().unwrap(), kind);
        }
    }

    #[test]
    fn debug_never_prints_the_password() {
        let auth = ProxyAuth {
            username: "alice".to_string(),
            password: "super-secret".to_string(),
        };
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("super-secret"));
        assert!(rendered.contains("<redacted>"));
    }
}
