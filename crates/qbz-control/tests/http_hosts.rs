//! Real loopback HTTP against two independent host fixtures. No audio/runtime,
//! account, machine profile or external service is used.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use qbz_control::{bind, serve, ApiHandle, HttpHost};
use qbz_models::CoreEvent;
use tiny_http::{Request, Response};
use tokio::sync::broadcast;

struct Host {
    name: &'static str,
    token: Option<String>,
    bus: broadcast::Sender<CoreEvent>,
    calls: Arc<AtomicUsize>,
    dropped: std::sync::mpsc::Sender<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}
impl HttpHost for Host {
    fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
    fn events(&self) -> &broadcast::Sender<CoreEvent> {
        &self.bus
    }
    fn route(&self, req: &mut Request) -> Response<std::io::Cursor<Vec<u8>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match req.url().split('?').next().unwrap() {
            "/api/ping" => qbz_control::json(200, serde_json::json!({"app":self.name})),
            "/api/echo" => {
                let mut data = String::new();
                req.as_reader().read_to_string(&mut data).unwrap();
                qbz_control::json(200, serde_json::json!({"app":self.name,"data":data}))
            }
            _ => qbz_control::err_json(404, "not_found", "unknown route", "fixture"),
        }
    }
}
struct Fixture {
    addr: SocketAddr,
    handle: Option<ApiHandle>,
    bus: broadcast::Sender<CoreEvent>,
    calls: Arc<AtomicUsize>,
    dropped: std::sync::mpsc::Receiver<()>,
}
impl Fixture {
    fn start(name: &'static str, token: Option<&str>) -> Self {
        let bound = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = bound.local_addr();
        let (bus, _) = broadcast::channel(16);
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, dropped) = std::sync::mpsc::channel();
        let handle = serve(
            bound,
            Host {
                name,
                token: token.map(str::to_owned),
                bus: bus.clone(),
                calls: calls.clone(),
                dropped: tx,
            },
        );
        Self {
            addr,
            handle: Some(handle),
            bus,
            calls,
            dropped,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
        }
    }
}
fn connect(addr: SocketAddr, method: &str, path: &str, headers: &str, body: &str) -> TcpStream {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    stream
}
fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &str,
    body: &str,
) -> (u16, serde_json::Value) {
    let mut stream = connect(addr, method, path, headers, body);
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = text.split_once("\r\n\r\n").unwrap().1;
    (status, serde_json::from_str(body).unwrap())
}

#[test]
fn two_hosts_route_to_their_own_state_and_gate_before_dispatch() {
    let a = Fixture::start("first", Some("alpha"));
    let b = Fixture::start("second", Some("beta"));
    assert_eq!(
        request(a.addr, "GET", "/api/ping?probe=1", "", "").1["app"],
        "first"
    );
    assert_eq!(
        request(b.addr, "GET", "/api/ping", "", "").1["app"],
        "second"
    );
    let before = a.calls.load(Ordering::SeqCst);
    for path in ["/api/echo", "/api/events", "/missing"] {
        let (status, body) = request(a.addr, "GET", path, "Authorization: Bearer beta\r\n", "");
        assert_eq!(status, 401);
        assert_eq!(body["error"]["code"], "invalid_token");
    }
    for path in ["/api/ping", "/api/echo", "/api/events"] {
        let (status, body) = request(
            a.addr,
            "GET",
            path,
            "Origin: https://example.invalid\r\nAuthorization: Bearer alpha\r\n",
            "",
        );
        assert_eq!(status, 403);
        assert_eq!(body["error"]["code"], "origin_forbidden");
    }
    assert_eq!(a.calls.load(Ordering::SeqCst), before);
    assert_eq!(a.bus.receiver_count(), 0);
    let (status, echo) = request(
        a.addr,
        "POST",
        "/api/echo",
        "Authorization: Bearer alpha\r\n",
        "payload",
    );
    assert_eq!(status, 200);
    assert_eq!(echo, serde_json::json!({"app":"first","data":"payload"}));
    assert_eq!(
        request(
            b.addr,
            "GET",
            "/missing",
            "Authorization: Bearer beta\r\n",
            ""
        )
        .0,
        404
    );
}

#[test]
fn sse_uses_selected_host_bus_without_blocking_regular_requests() {
    let a = Fixture::start("first", None);
    let b = Fixture::start("second", None);
    let stream = connect(a.addr, "GET", "/api/events", "", "");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("HTTP/1.1 200"));
    let mut headers = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        assert!(!line.is_empty(), "SSE closed before headers finished");
        headers.push_str(&line);
    }
    assert!(headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked"));
    // The priming comment must reach the socket even before the first event.
    assert_eq!(read_chunk(&mut reader), ": qbzd event stream\n\n");
    assert_eq!(a.bus.receiver_count(), 1);
    assert_eq!(b.bus.receiver_count(), 0);
    assert_eq!(request(a.addr, "GET", "/api/ping", "", "").0, 200);
    a.bus
        .send(CoreEvent::VolumeChanged { volume: 0.25 })
        .unwrap();
    let volume_frame = read_chunk(&mut reader);
    assert!(volume_frame.starts_with("event: VolumeChanged\n"));
    assert!(volume_frame.ends_with("\n\n"));
    let data = volume_frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(data).unwrap(),
        serde_json::json!({"type": "VolumeChanged", "data": {"volume": 0.25}})
    );
    a.bus
        .send(CoreEvent::LoggedIn {
            session: qbz_models::UserSession {
                user_id: 5,
                display_name: "Música".into(),
                user_auth_token: "private-fixture-token".into(),
                ..Default::default()
            },
        })
        .unwrap();
    let frame = read_chunk(&mut reader);
    assert!(frame.contains("Música"));
    assert!(!frame.contains("private-fixture-token"));
    assert!(!frame.contains("user_auth_token"));
}

fn read_chunk(reader: &mut impl BufRead) -> String {
    let mut size = String::new();
    reader.read_line(&mut size).unwrap();
    let len = usize::from_str_radix(size.trim(), 16).unwrap();
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).unwrap();
    let mut end = [0u8; 2];
    reader.read_exact(&mut end).unwrap();
    assert_eq!(&end, b"\r\n");
    String::from_utf8(bytes).unwrap()
}

#[test]
fn shutdown_releases_host_state_and_listener() {
    let mut fixture = Fixture::start("first", None);
    assert_eq!(request(fixture.addr, "GET", "/api/ping", "", "").0, 200);
    fixture.handle.take().unwrap().shutdown();
    fixture
        .dropped
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    // tiny_http wakes its background accept thread on Drop but does not join
    // it. Host state must be gone above; allow that documented asynchronous
    // socket close to finish, without an unconditional sleep or unbounded retry.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match bind(fixture.addr) {
            Ok(rebound) => {
                drop(rebound);
                break;
            }
            Err(qbz_control::BindError::AddrInUse(_)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("listener not released after shutdown: {error:?}"),
        }
    }
}
