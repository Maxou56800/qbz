//! Real HTTP + real scanner: admission, host ownership and playable-file metadata.
use qbz_control::library::LibraryHost;
use qbz_library::{service::LibraryService, LibraryStore};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};

fn request(addr: SocketAddr, method: &str, path: &str, token: &str, body: Value) -> (u16, Value) {
    let mut socket = TcpStream::connect(addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let body = body.to_string();
    write!(socket, "{method} {path} HTTP/1.0\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    (
        headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
fn wav(path: &std::path::Path) {
    let data = vec![0u8; 88200];
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(44100u32.to_le_bytes());
    bytes.extend(88200u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn administration_scans_only_the_named_host_and_rejects_stale_commands() {
    let temp = tempfile::tempdir().unwrap();
    let mut hosts = Vec::new();
    for name in ["a", "b"] {
        let root = temp.path().join(name);
        let service = Arc::new(LibraryService::new(
            LibraryStore::new(root.join("library.db")),
            root.join("artwork"),
        ));
        let bound = qbz_control::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = bound.local_addr();
        let server = qbz_control::serve(
            bound,
            LibraryHost::with_management(service.clone(), name.into(), name.into(), true),
        );
        let (code, info) = request(addr, "GET", "/api/orbit/library", name, Value::Null);
        assert_eq!(code, 200);
        assert!(info["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("library.files.manage")));
        hosts.push((server, service, addr, info));
    }
    let (_, service, addr, info) = &hosts[0];
    let music = temp.path().join("Music");
    std::fs::create_dir(&music).unwrap();
    wav(&music.join("Orbit sample.wav"));
    let add = json!({"instance":info["instance"], "revision":0, "path":music.to_str().unwrap()});
    assert_eq!(
        request(
            *addr,
            "POST",
            "/api/orbit/library/folders",
            "wrong",
            add.clone()
        )
        .0,
        401
    );
    let mut wrong = add.clone();
    wrong["instance"] = hosts[1].3["instance"].clone();
    assert_eq!(
        request(*addr, "POST", "/api/orbit/library/folders", "a", wrong).0,
        409
    );
    assert!(!service.store().database_path().exists());
    let mut invalid = add.clone();
    invalid["path"] = json!("relative");
    assert_eq!(
        request(*addr, "POST", "/api/orbit/library/folders", "a", invalid).0,
        400
    );
    let (code, folders) = request(
        *addr,
        "POST",
        "/api/orbit/library/folders",
        "a",
        add.clone(),
    );
    assert_eq!(code, 200, "{folders}");
    assert_eq!(folders["revision"], 1);
    assert_eq!(folders["folders"].as_array().unwrap().len(), 1);
    assert_eq!(
        request(*addr, "POST", "/api/orbit/library/folders", "a", add).0,
        409
    );
    assert!(!hosts[1].1.store().database_path().exists());
    let scan = json!({"instance":info["instance"], "folder_id":folders["folders"][0]["id"]});
    let mut missing = scan.clone();
    missing["folder_id"] = json!(9999);
    assert_eq!(
        request(*addr, "POST", "/api/orbit/library/scan", "a", missing).0,
        404
    );
    assert_eq!(
        request(*addr, "POST", "/api/orbit/library/scan", "a", scan).0,
        202
    );
    assert!(service.wait_idle(Duration::from_secs(10)));
    let (_, after) = request(*addr, "GET", "/api/orbit/library", "a", Value::Null);
    assert_eq!(after["tracks"], 1, "{after}");
    assert_eq!(after["revision"], 2);
    let instance = info["instance"].as_str().unwrap();
    let (_, page) = request(
        *addr,
        "GET",
        &format!("/api/orbit/library/search?instance={instance}&q=Orbit"),
        "a",
        Value::Null,
    );
    assert_eq!(page["tracks"].as_array().unwrap().len(), 1);
    assert!(!page.to_string().contains(music.to_str().unwrap()));
    let (_, jobs) = request(
        *addr,
        "GET",
        &format!("/api/orbit/library/jobs?instance={instance}"),
        "a",
        Value::Null,
    );
    assert_eq!(jobs["library"]["last_scan"]["outcome"], "complete");
    assert_eq!(
        request(
            *addr,
            "POST",
            "/api/orbit/library/cancel",
            "a",
            json!({"instance":instance,"job":1})
        )
        .0,
        409
    );
    for (server, service, _, _) in hosts {
        server.shutdown();
        service.shutdown();
    }
}

#[test]
fn inspection_does_not_grant_administration_and_bad_bodies_never_create_a_profile() {
    let temp = tempfile::tempdir().unwrap();
    for management in [false, true] {
        let root = temp.path().join(management.to_string());
        let service = Arc::new(LibraryService::new(
            LibraryStore::new(root.join("library.db")),
            root.join("artwork"),
        ));
        let bound = qbz_control::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = bound.local_addr();
        let server = qbz_control::serve(
            bound,
            LibraryHost::with_management(
                service.clone(),
                "fixture".into(),
                "key".into(),
                management,
            ),
        );
        let (_, info) = request(addr, "GET", "/api/orbit/library", "key", Value::Null);
        for body in [
            Value::Null,
            json!({"instance":info["instance"],"unknown":true}),
            json!({"instance":info["instance"],"path":"x".repeat(9000),"revision":0}),
        ] {
            assert_eq!(
                request(addr, "POST", "/api/orbit/library/folders", "key", body).0,
                if management { 400 } else { 404 }
            );
        }
        assert!(!root.exists());
        server.shutdown();
        service.shutdown();
    }
}
