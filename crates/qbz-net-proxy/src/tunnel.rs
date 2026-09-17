//! Raw, proxy-tunneled streams for the one consumer that doesn't use
//! reqwest: `qconnect-transport-ws`'s WebSocket connects with
//! `tokio-tungstenite`, which dials its own TCP/TLS and has no proxy support
//! at all. [`connect_tunnel`] establishes the tunnel first — through SOCKS4,
//! SOCKS5 or a plain HTTP `CONNECT` — and hands back a plain
//! `AsyncRead + AsyncWrite` stream that the caller then passes to
//! `tokio_tungstenite::client_async_tls_with_config` in place of a fresh
//! `TcpStream::connect`.
//!
//! Domain targets are passed straight through to the proxy (never resolved
//! locally first) for the same reason [`crate::ProxyKind::url_scheme`]
//! always builds `socks4a`/`socks5h`: resolving locally leaks every host
//! visited to the local network/ISP even though the traffic itself is
//! proxied.
//!
//! An HTTPS proxy (TLS to the proxy itself, as opposed to the `wss://`
//! target's own TLS layer) is not supported here — see [`connect_tunnel`].
//! reqwest's own proxy support (used by every other client in the
//! workspace) is unaffected; this module exists only for the one channel
//! that bypasses reqwest entirely.

use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{ProxyAuth, ProxyConfig, ProxyConfigError, ProxyKind};

/// An established tunnel, ready to carry the target's own protocol (TLS,
/// then WebSocket). Boxed because SOCKS and HTTP `CONNECT` tunnels are
/// different concrete stream types.
pub trait TunnelStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> TunnelStream for T {}

/// Longest HTTP `CONNECT` response this will buffer before giving up, so a
/// proxy that never sends a blank line can't grow this without bound.
const MAX_CONNECT_RESPONSE_BYTES: usize = 8 * 1024;

/// Establish a tunnel to `target_host:target_port` through `config`.
///
/// [`ProxyKind::Https`] is rejected: that variant means "the proxy itself
/// requires TLS to connect to", a second, independent TLS layer this crate
/// has no reason to hand-roll for the single consumer that needs a tunnel
/// at all. Configure the proxy as [`ProxyKind::Http`] (the ordinary case —
/// almost no deployed proxy actually requires the client to open TLS to the
/// proxy itself) or a SOCKS kind instead.
pub async fn connect_tunnel(
    config: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> Result<Pin<Box<dyn TunnelStream>>, ProxyConfigError> {
    match config.kind {
        ProxyKind::Socks5 => {
            let stream = connect_socks5(config, target_host, target_port).await?;
            Ok(Box::pin(stream))
        }
        ProxyKind::Socks4 => {
            let stream = connect_socks4(config, target_host, target_port).await?;
            Ok(Box::pin(stream))
        }
        ProxyKind::Http => {
            let stream = connect_http(config, target_host, target_port).await?;
            Ok(Box::pin(stream))
        }
        ProxyKind::Https => Err(ProxyConfigError::Tunnel(
            "an HTTPS proxy (TLS to the proxy itself) is not supported for this channel; \
             use a plain HTTP or SOCKS proxy"
                .to_string(),
        )),
    }
}

async fn connect_socks5(
    config: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> Result<tokio_socks::tcp::Socks5Stream<TcpStream>, ProxyConfigError> {
    let proxy = (config.host.as_str(), config.port);
    let target = (target_host, target_port);
    let result = match &config.auth {
        Some(ProxyAuth { username, password }) if !username.is_empty() || !password.is_empty() => {
            tokio_socks::tcp::Socks5Stream::connect_with_password(proxy, target, username, password)
                .await
        }
        _ => tokio_socks::tcp::Socks5Stream::connect(proxy, target).await,
    };
    result.map_err(|e| ProxyConfigError::Tunnel(format!("SOCKS5 tunnel failed: {e}")))
}

async fn connect_socks4(
    config: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> Result<tokio_socks::tcp::Socks4Stream<TcpStream>, ProxyConfigError> {
    let proxy = (config.host.as_str(), config.port);
    let target = (target_host, target_port);
    // SOCKS4 has only a single USERID field — no password. A configured
    // password is silently unusable here; the settings UI should say so for
    // this proxy kind rather than this layer failing loudly for a field the
    // protocol simply has no room for.
    let result = match &config.auth {
        Some(ProxyAuth { username, .. }) if !username.is_empty() => {
            tokio_socks::tcp::Socks4Stream::connect_with_userid(proxy, target, username).await
        }
        _ => tokio_socks::tcp::Socks4Stream::connect(proxy, target).await,
    };
    result.map_err(|e| ProxyConfigError::Tunnel(format!("SOCKS4 tunnel failed: {e}")))
}

async fn connect_http(
    config: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, ProxyConfigError> {
    use base64::Engine;

    let mut stream = TcpStream::connect((config.host.as_str(), config.port))
        .await
        .map_err(|e| ProxyConfigError::Tunnel(format!("connect to proxy: {e}")))?;

    let mut request = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
         Host: {target_host}:{target_port}\r\n"
    );
    if let Some(auth) = &config.auth {
        if !auth.username.is_empty() || !auth.password.is_empty() {
            let credentials = base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", auth.username, auth.password));
            request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
        }
    }
    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| ProxyConfigError::Tunnel(format!("send CONNECT request: {e}")))?;

    let status_line = read_connect_response(&mut stream).await?;
    // "HTTP/1.1 200 Connection established" — accept any 2xx, matching how
    // real proxies phrase the reason (Squid/nginx/etc. all differ).
    let status_ok = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (200..300).contains(&code));
    if !status_ok {
        return Err(ProxyConfigError::Tunnel(format!(
            "proxy refused CONNECT: {}",
            status_line.trim()
        )));
    }

    Ok(stream)
}

/// Read until the blank line that ends the CONNECT response's headers,
/// bounded by [`MAX_CONNECT_RESPONSE_BYTES`], and return just the status
/// line. Any bytes the proxy sent past the header block belong to the
/// tunneled protocol (the target's TLS ServerHello) and must not be
/// consumed — a well-behaved HTTP proxy never pipelines them before the
/// tunnel is confirmed, so reading one byte at a time here costs nothing in
/// practice and avoids the complexity of pushing read-ahead bytes back.
async fn read_connect_response(stream: &mut TcpStream) -> Result<String, ProxyConfigError> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if buffer.len() >= MAX_CONNECT_RESPONSE_BYTES {
            return Err(ProxyConfigError::Tunnel(
                "proxy CONNECT response too large".to_string(),
            ));
        }
        let read = stream
            .read(&mut byte)
            .await
            .map_err(|e| ProxyConfigError::Tunnel(format!("read CONNECT response: {e}")))?;
        if read == 0 {
            return Err(ProxyConfigError::Tunnel(
                "proxy closed the connection during CONNECT".to_string(),
            ));
        }
        buffer.push(byte[0]);
        if buffer.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    Ok(text.lines().next().unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn http_proxy_config(port: u16, auth: Option<ProxyAuth>) -> ProxyConfig {
        ProxyConfig {
            kind: ProxyKind::Http,
            host: "127.0.0.1".to_string(),
            port,
            auth,
            insecure_tls: false,
        }
    }

    /// Accepts one connection, reads until the blank line, then responds and
    /// hands control to `after` to finish the exchange over the tunnel.
    async fn fake_http_proxy(
        listener: TcpListener,
        response: &'static str,
        after: impl FnOnce(TcpStream) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + 'static,
    ) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte).await.unwrap();
            buffer.push(byte[0]);
            if buffer.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        stream.write_all(response.as_bytes()).await.unwrap();
        after(stream).await;
    }

    #[tokio::test]
    async fn http_connect_accepts_any_2xx_and_tunnels_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(fake_http_proxy(
            listener,
            "HTTP/1.1 200 Connection established\r\n\r\n",
            |mut stream| {
                Box::pin(async move {
                    let mut buf = [0u8; 5];
                    stream.read_exact(&mut buf).await.unwrap();
                    assert_eq!(&buf, b"hello");
                })
            },
        ));

        let mut tunneled =
            match connect_tunnel(&http_proxy_config(port, None), "example.invalid", 443).await {
                Ok(t) => t,
                Err(e) => panic!("tunnel established: {e}"),
            };
        tunneled.write_all(b"hello").await.unwrap();
    }

    #[tokio::test]
    async fn http_connect_rejects_a_non_2xx_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(fake_http_proxy(
            listener,
            "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
            |_stream| Box::pin(async move {}),
        ));

        let err = match connect_tunnel(&http_proxy_config(port, None), "example.invalid", 443).await
        {
            Err(e) => e,
            Ok(_) => panic!("a 407 must not be treated as a successful tunnel"),
        };
        assert!(matches!(err, ProxyConfigError::Tunnel(_)));
    }

    #[tokio::test]
    async fn http_connect_sends_basic_proxy_auth_when_configured() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                stream.read_exact(&mut byte).await.unwrap();
                buffer.push(byte[0]);
                if buffer.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buffer).into_owned();
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let _ = tx.send(request);
        });

        let auth = ProxyAuth {
            username: "alice".to_string(),
            password: "hunter2".to_string(),
        };
        let _tunneled = match connect_tunnel(
            &http_proxy_config(port, Some(auth)),
            "example.invalid",
            443,
        )
        .await
        {
            Ok(t) => t,
            Err(e) => panic!("tunnel established: {e}"),
        };

        let request = rx.await.unwrap();
        // base64("alice:hunter2") = YWxpY2U6aHVudGVyMg==
        assert!(request.contains("Proxy-Authorization: Basic YWxpY2U6aHVudGVyMg==\r\n"));
    }

    #[tokio::test]
    async fn https_proxy_kind_is_rejected_before_touching_the_network() {
        let cfg = ProxyConfig {
            kind: ProxyKind::Https,
            host: "127.0.0.1".to_string(),
            port: 1,
            auth: None,
            insecure_tls: false,
        };
        let err = match connect_tunnel(&cfg, "example.invalid", 443).await {
            Err(e) => e,
            Ok(_) => panic!("HTTPS-to-the-proxy must be rejected, not silently attempted"),
        };
        assert!(matches!(err, ProxyConfigError::Tunnel(_)));
    }
}
