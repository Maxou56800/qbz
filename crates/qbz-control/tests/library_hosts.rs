//! Orbit inspection against real independent profile databases and sockets.
use qbz_control::library::LibraryHost;
use qbz_library::{service::LibraryService, LibraryStore, LocalTrack};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

fn request(addr: SocketAddr, path: &str, token: &str) -> (u16, serde_json::Value) {
    let mut socket = TcpStream::connect(addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        socket,
        "GET {path} HTTP/1.0\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    (
        headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}

#[test]
fn two_hosts_keep_search_identity_paths_and_access_keys_separate() {
    let temp = tempfile::tempdir().unwrap();
    let mut hosts = Vec::new();
    for name in ["desktop", "daemon"] {
        let root = temp.path().join(name);
        let service = Arc::new(LibraryService::new(
            LibraryStore::new(root.join("library.db")),
            root.join("artwork"),
        ));
        service
            .store()
            .write(|db| {
                for n in 0..3 {
                    db.insert_track(&LocalTrack {
                        file_path: format!("/private/{name}/secret-{n}.flac"),
                        title: format!("Shared title {n}"),
                        artist: name.into(),
                        album: "Fixture".into(),
                        artwork_path: Some("/private/artwork.jpg".into()),
                        ..Default::default()
                    })?;
                }
                Ok(())
            })
            .unwrap();
        let bound = qbz_control::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = bound.local_addr();
        let token = format!("{name}-private-access-key");
        let handle = qbz_control::serve(
            bound,
            LibraryHost::new(service.clone(), name.into(), token.clone()),
        );
        let (code, info) = request(addr, "/api/orbit/library", &token);
        assert_eq!(code, 200);
        assert_eq!(info["tracks"], 3);
        assert_eq!(
            info["capabilities"],
            serde_json::json!(["library.files.inspect", "library.files.search"])
        );
        hosts.push((handle, service, addr, token, info));
    }
    assert_ne!(hosts[0].4["instance"], hosts[1].4["instance"]);
    for (index, name) in ["desktop", "daemon"].into_iter().enumerate() {
        let (_, service, addr, token, info) = &hosts[index];
        let instance = info["instance"].as_str().unwrap();
        let path = format!("/api/orbit/library/search?q=Shared+title&limit=2&instance={instance}");
        assert_eq!(request(*addr, &path, &hosts[1 - index].3).0, 401);
        let (code, page) = request(*addr, &path, token);
        assert_eq!(code, 200);
        assert_eq!(page["tracks"].as_array().unwrap().len(), 2);
        assert_eq!(page["tracks"][0]["artist"], name);
        assert_eq!(page["has_more"], true);
        let raw = page.to_string();
        assert!(!raw.contains("/private"));
        assert!(!raw.contains("access-key"));
        assert_eq!(
            request(*addr, &path.replace("limit=2", "limit=100000"), token).0,
            400
        );
        assert_eq!(
            request(
                *addr,
                &path.replace(instance, hosts[1 - index].4["instance"].as_str().unwrap()),
                token
            )
            .0,
            409
        );
        service.close();
        assert_eq!(request(*addr, &path, token).0, 409);
    }
    for (handle, service, _, _, _) in hosts {
        handle.shutdown();
        service.shutdown();
    }
}

#[test]
fn reading_an_empty_host_needs_no_qobuz_session_and_creates_no_profile() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("uncreated");
    let service = Arc::new(LibraryService::new(
        LibraryStore::new(root.join("library.db")),
        root.join("artwork"),
    ));
    let bound = qbz_control::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = bound.local_addr();
    let server = qbz_control::serve(
        bound,
        LibraryHost::new(service.clone(), "Guest".into(), "test-key".into()),
    );
    let (code, info) = request(addr, "/api/orbit/library", "test-key");
    assert_eq!(code, 200);
    assert_eq!(info["tracks"], 0);
    assert!(!root.exists());
    server.shutdown();
    service.shutdown();
}
