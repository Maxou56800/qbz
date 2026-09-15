//! Wire-level authorization contract (#502). Jellyfin 12 no longer reads the
//! legacy token forms, so every authenticated request must carry the token in
//! `Authorization: MediaBrowser …, Token="…"` — and nothing else. A local socket
//! records exactly what the client sends; CI runs this, unlike `tests/live.rs`.
use qbz_jellyfin::{JellyfinClient, JellyfinError};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const VIEWS: &str =
    r#"{"Items":[{"Id":"lib","Name":"Music","CollectionType":"music"}],"TotalRecordCount":1}"#;

/// Serve `responses` in order; hand back every raw request received.
async fn server(
    responses: Vec<(u16, &'static str)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/jellyfin", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            while !String::from_utf8_lossy(&bytes).contains("\r\n\r\n") {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            requests.push(String::from_utf8(bytes).unwrap());
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    (url, task)
}

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn assert_modern_authorization(request: &str) {
    let auth = header(request, "authorization").expect("no Authorization header");
    assert!(auth.starts_with("MediaBrowser "), "{auth}");
    assert!(auth.contains(r#"Token="stored-token""#), "{auth}");
    assert!(auth.contains(r#"DeviceId="stable-install""#), "{auth}");
    for legacy in [
        "x-emby-token",
        "x-mediabrowser-token",
        "x-emby-authorization",
    ] {
        assert!(
            header(request, legacy).is_none(),
            "legacy header {legacy} sent"
        );
    }
    assert!(!request.contains("api_key"), "legacy api_key query sent");
}

fn client(url: &str) -> JellyfinClient {
    JellyfinClient::new(url, "stored-token", "user-1", "stable-install").unwrap()
}

#[tokio::test]
async fn library_reads_use_the_authorization_header_and_the_current_views_route() {
    let (url, task) = server(vec![
        (200, VIEWS),
        (200, r#"{"Items":[],"TotalRecordCount":7}"#),
        (200, r#"{"Items":[],"TotalRecordCount":7}"#),
        (
            200,
            r#"{"Items":[{"Id":"t1","Container":"flac"}],"TotalRecordCount":1}"#,
        ),
    ])
    .await;
    let c = client(&url);
    assert_eq!(c.music_libraries().await.unwrap()[0].id, "lib");
    assert_eq!(c.track_count(Some("lib")).await.unwrap(), 7);
    assert_eq!(
        c.essential_tracks_page(Some("lib"), 0, None)
            .await
            .unwrap()
            .1,
        7
    );
    assert_eq!(
        c.track_quality(&["t1".to_string()]).await.unwrap()[0].id,
        "t1"
    );
    let requests = task.await.unwrap();
    assert!(
        requests[0].starts_with("GET /jellyfin/UserViews?userId=user-1 HTTP/1.1"),
        "{}",
        requests[0].lines().next().unwrap()
    );
    for request in &requests {
        assert_modern_authorization(request);
    }
}

/// 10.8 has no `/UserViews`; only then is the obsolete route asked.
#[tokio::test]
async fn a_server_without_user_views_falls_back_to_the_obsolete_route() {
    let (url, task) = server(vec![(404, ""), (200, VIEWS)]).await;
    assert_eq!(client(&url).music_libraries().await.unwrap().len(), 1);
    let requests = task.await.unwrap();
    assert!(requests[1].starts_with("GET /jellyfin/Users/user-1/Views HTTP/1.1"));
    assert_modern_authorization(&requests[1]);
}

#[tokio::test]
async fn a_rejected_token_is_unauthorized_and_does_not_fall_back() {
    let (url, task) = server(vec![(401, "")]).await;
    assert_eq!(
        client(&url).music_libraries().await.unwrap_err(),
        JellyfinError::Unauthorized
    );
    assert_eq!(task.await.unwrap().len(), 1);
}

#[test]
fn the_stream_url_uses_the_modern_query_key() {
    let url = qbz_jellyfin::stream_url("http://h:8096", "stored-token", "item");
    assert_eq!(
        url,
        "http://h:8096/Audio/item/stream?static=true&ApiKey=stored-token"
    );
}
