//! URL-free diagnostics for reqwest failures.
//!
//! `reqwest::Error`'s `Display` embeds the full request URL, and the URLs this
//! crate fetches carry signed, short-lived CDN tokens (`hmac=` on the Qobuz
//! file CDN, `Signature=` / `Key-Pair-Id=` on CloudFront). Nothing in a log
//! line or a returned error string may carry them. Moved here from
//! `qbz-player/src/remote_stream.rs` so the CMAF fetch path
//! (`retry::classify_reqwest`) uses the same helper as the legacy stream path.

/// Return a bounded, URL-free diagnostic for a reqwest failure.
///
/// The raw error and its source chain can contain a signed CDN URL. Inspect the
/// chain only to preserve header-limit classification; never copy an arbitrary
/// cause into logs or a returned error.
pub fn describe_reqwest_error(err: &reqwest::Error) -> String {
    if error_chain_has_header_limit(err) {
        return safe_transport_diagnostic("message head is too large").to_string();
    }

    if err.is_timeout() {
        "HTTP transport timed out".to_string()
    } else if err.is_connect() {
        "HTTP transport connection failed".to_string()
    } else if err.is_body() {
        "HTTP response body failed".to_string()
    } else if err.is_decode() {
        "HTTP response decode failed".to_string()
    } else if err.is_status() {
        "HTTP status rejected".to_string()
    } else {
        "HTTP transport request failed".to_string()
    }
}

fn error_chain_has_header_limit(err: &reqwest::Error) -> bool {
    use std::error::Error as _;

    if is_header_flood_error(&err.to_string()) {
        return true;
    }
    let mut source = err.source();
    while let Some(cause) = source {
        if is_header_flood_error(&cause.to_string()) {
            return true;
        }
        source = cause.source();
    }
    false
}

fn safe_transport_diagnostic(message: &str) -> &'static str {
    if is_header_flood_error(message) {
        "HTTP response header limit exceeded (message head is too large)"
    } else {
        "HTTP transport request failed"
    }
}

/// True when an error message (already chain-expanded by
/// [`describe_reqwest_error`]) shows hyper's hard-coded h1 100-header cap.
/// Akamai answers SMALL raw-url objects with ~106 headers (the `X-AK-GRN` /
/// `X-AK-FWD-ERROR: ERR_POC_FWD_OBJ_TOO_SMALL` flood), so EVERY reqwest fetch
/// of such an URL fails this way — streaming probe and full download alike.
pub fn is_header_flood_error(message: &str) -> bool {
    let haystack = message.to_ascii_lowercase();
    haystack.contains("message head is too large") || haystack.contains("too many headers")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_diagnostic_never_echoes_signed_url() {
        let marker = "https://cdn.example/audio.flac?jwt=secret&request_sig=signed";
        let diagnostic = safe_transport_diagnostic(marker);
        assert_eq!(diagnostic, "HTTP transport request failed");
        assert!(!diagnostic.contains(marker));
        assert!(!diagnostic.contains("secret"));
    }

    /// The CMAF path: a real reqwest failure (connection refused) whose
    /// Display embeds the signed URL must classify without it.
    #[tokio::test]
    async fn classify_reqwest_never_echoes_the_request_url() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        }; // dropped: nothing listens on `port` now
        let url = format!("http://127.0.0.1:{port}/file?eid=1&hmac=SIGNEDSECRET&etsp=1");
        let err = reqwest::Client::new().get(&url).send().await.unwrap_err();
        assert!(
            err.to_string().contains("SIGNEDSECRET"),
            "precondition: reqwest's Display embeds the url: {err}"
        );

        assert_eq!(describe_reqwest_error(&err), "HTTP transport connection failed");
        let classified = crate::retry::classify_reqwest(&err, "fetch").to_string();
        assert_eq!(classified, "fetch: HTTP transport connection failed");
        assert!(!classified.contains("SIGNEDSECRET"));
        assert!(!classified.contains("127.0.0.1"));
    }
}
