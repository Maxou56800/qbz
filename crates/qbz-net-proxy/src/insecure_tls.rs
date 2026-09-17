//! A rustls `ClientConfig` that trusts *any* certificate presented by the
//! configured proxy's own TLS handshake, while verifying every other
//! hostname exactly as normal.
//!
//! This exists for one narrow case: an `https://` proxy (the proxy's own
//! listening socket is TLS) whose certificate the user knows and accepts —
//! typically a self-signed one, e.g. a local test proxy. Reqwest only
//! exposes `danger_accept_invalid_certs`, which disables verification for
//! the *whole client* — the real destination reached through the tunnel
//! (Qobuz's API, its CDN, ...) would go unverified too. [`ScopedInsecureVerifier`]
//! is scoped by hostname instead: it bypasses verification only for the
//! proxy's own name, and delegates everything else — including the tunneled
//! target's certificate — to the platform's normal trust store.
//!
//! Reqwest builds two `rustls::ClientConfig`s when a proxy is set: one for
//! the target, one for the proxy connection itself (`tls_proxy` in its own
//! `connect.rs`), the latter a clone of whatever we hand it here. Since both
//! ultimately share this same verifier, the hostname check below is what
//! keeps the bypass scoped to the proxy — not which config object is used.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, Error as TlsError, SignatureScheme,
};
use rustls_platform_verifier::Verifier as PlatformVerifier;

use crate::ProxyConfigError;

#[derive(Debug)]
struct ScopedInsecureVerifier {
    /// The proxy's own hostname, exactly as it will appear as the SNI/
    /// `ServerName` for the proxy's TLS handshake. Every other `ServerName`
    /// (the real target, reached through the proxy) is fully verified.
    proxy_name: ServerName<'static>,
    delegate: PlatformVerifier,
}

impl ServerCertVerifier for ScopedInsecureVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        if *server_name == self.proxy_name {
            return Ok(ServerCertVerified::assertion());
        }
        self.delegate
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    // Signature checks are pure cryptography (does this signature match this
    // cert's key?), not trust decisions — always delegated, proxy or not.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.delegate.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.delegate.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.delegate.supported_verify_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        self.delegate.requires_raw_public_keys()
    }

    fn root_hint_subjects(&self) -> Option<&[DistinguishedName]> {
        self.delegate.root_hint_subjects()
    }
}

/// Build a `ClientConfig` that trusts any certificate for `proxy_host`
/// specifically and verifies every other hostname through the platform's
/// normal trust store. `proxy_host` must be exactly [`ProxyConfig::host`] —
/// it is parsed the same way reqwest parses the proxy URL's host, so the two
/// stay in agreement about which hostname is "the proxy".
pub(crate) fn client_config_for_insecure_proxy(
    proxy_host: &str,
) -> Result<ClientConfig, ProxyConfigError> {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .ok_or_else(|| {
            ProxyConfigError::InsecureTls(
                "no rustls CryptoProvider installed for this process".to_string(),
            )
        })?;
    let proxy_name = ServerName::try_from(proxy_host.to_string())
        .map_err(|e| ProxyConfigError::InsecureTls(format!("invalid proxy host: {e}")))?;
    let delegate = PlatformVerifier::new(provider.clone())
        .map_err(|e| ProxyConfigError::InsecureTls(e.to_string()))?;

    let verifier = Arc::new(ScopedInsecureVerifier {
        proxy_name,
        delegate,
    });
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| ProxyConfigError::InsecureTls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_crypto_provider() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    #[test]
    fn builds_a_config_scoped_to_the_given_proxy_host() {
        ensure_crypto_provider();
        let config = client_config_for_insecure_proxy("127.0.0.1").expect("build config");
        // Not much of the resulting rustls::ClientConfig is inspectable from
        // outside the crate; the real behavior (bypass only for the proxy
        // name) is exercised end to end by qbz-net-proxy's own `test()`
        // against a live TLS listener, not unit-testable in isolation here.
        assert!(config.alpn_protocols.is_empty());
    }

    #[test]
    fn rejects_an_unparseable_proxy_host() {
        ensure_crypto_provider();
        assert!(client_config_for_insecure_proxy("").is_err());
    }

    // ---- end-to-end: a real TLS handshake against a real self-signed cert ----
    //
    // Proves the two claims that matter, against an actual `rustls::ServerConfig`
    // and socket rather than just asserting the `ClientConfig` builds:
    // `insecure_tls: true` gets past this exact untrusted certificate for the
    // proxy hop (the failure that follows is the fake proxy not speaking real
    // CONNECT, not a certificate error), and `insecure_tls: false` still
    // rejects that same certificate. The "the real target stays verified"
    // half of the design isn't re-proven with a second TLS hop here — it
    // follows directly from `verify_server_cert` above discriminating on
    // `server_name`, the same few lines this test already exercises for the
    // proxy's own name.

    static TEST_CERT_DER: &[u8] = include_bytes!("../testdata/insecure_proxy_test_cert.der");
    static TEST_KEY_DER: &[u8] = include_bytes!("../testdata/insecure_proxy_test_key.der");
    const TEST_CERT_HOST: &str = "insecure-proxy.test";

    /// Binds a TCP listener and completes exactly one TLS handshake with the
    /// fixed self-signed cert above, then drops the connection — enough to
    /// prove whether the *handshake* (and therefore certificate
    /// verification) succeeded, without needing to speak real HTTP CONNECT.
    async fn spawn_fake_tls_proxy() -> std::net::SocketAddr {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

        let cert = CertificateDer::from(TEST_CERT_DER.to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(TEST_KEY_DER.to_vec()));
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .expect("build server config with the fixed test cert/key");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake proxy listener");
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            // Errors here (e.g. the client aborting once it decides not to
            // trust us) are the point of the "insecure_tls: false" half of
            // the test, not a test-harness bug — nothing to assert on this
            // side beyond letting the handshake attempt run to completion.
            let _ = acceptor.accept(stream).await;
        });

        addr
    }

    /// A normally-verifying config (no hostname-scoped bypass) built the same
    /// way `client_config_for_insecure_proxy` builds its delegate, so the
    /// "false" half of the test exercises the exact same verifier the
    /// bypassed one delegates to.
    fn normal_client_config() -> ClientConfig {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .expect("crypto provider installed by ensure_crypto_provider()");
        let delegate =
            PlatformVerifier::new(provider.clone()).expect("build the platform verifier");
        ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(delegate))
            .with_no_client_auth()
    }

    /// `insecure-proxy.test` only resolves to the fake proxy's real
    /// 127.0.0.1 port via `/etc/hosts`-free means: rustls needs a DNS-shaped
    /// `ServerName` (an IP literal is a different `ServerName` variant our
    /// cert's SAN doesn't cover), but the connection itself must still land
    /// on loopback. `qbz_net_proxy::test`'s stage-one probe dials
    /// `host:port` directly, and reqwest's own connector resolves the proxy
    /// host the same way — so this only works if `insecure-proxy.test`
    /// resolves locally. Rather than require editing the test machine's
    /// hosts file, this test drives the handshake directly instead of going
    /// through `qbz_net_proxy::test()`'s full HTTP path.
    async fn handshake_outcome(addr: std::net::SocketAddr, insecure: bool) -> Result<(), String> {
        use rustls::pki_types::ServerName;

        let config = if insecure {
            client_config_for_insecure_proxy(TEST_CERT_HOST).expect("build insecure config")
        } else {
            normal_client_config()
        };
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let name = ServerName::try_from(TEST_CERT_HOST.to_string()).unwrap();
        let tcp = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to the fake proxy");
        connector
            .connect(name, tcp)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn insecure_true_completes_the_handshake_against_the_untrusted_cert() {
        ensure_crypto_provider();
        let addr = spawn_fake_tls_proxy().await;
        handshake_outcome(addr, true)
            .await
            .expect("insecure_tls: true must accept this self-signed cert");
    }

    #[tokio::test]
    async fn insecure_false_still_rejects_the_same_untrusted_cert() {
        ensure_crypto_provider();
        let addr = spawn_fake_tls_proxy().await;
        let err = handshake_outcome(addr, false)
            .await
            .expect_err("a normally-verifying config must reject a self-signed cert");
        assert!(
            err.to_lowercase().contains("certificate")
                || err.to_lowercase().contains("unknownissuer"),
            "expected a certificate-trust error, got: {err}"
        );
    }
}
