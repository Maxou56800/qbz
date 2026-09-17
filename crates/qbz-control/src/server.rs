use crate::{sse, wire::access_gate};
use qbz_models::CoreEvent;
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use tiny_http::{Method, Request, Response};
use tokio::sync::broadcast;

/// Host state lives on the single serving thread, so only Send is required.
/// Routing is called after the shared Origin/bearer gate. Host command handlers
/// remain responsible for playback authority and catalog-session admission.
pub trait HttpHost: Send + 'static {
    fn token(&self) -> Option<&str>;
    fn events(&self) -> &broadcast::Sender<CoreEvent>;
    fn route(&self, request: &mut Request) -> Response<Cursor<Vec<u8>>>;
}

/// A socket bound at boot step 5, not yet serving. Wraps the tiny_http server
/// in an `Arc` so the serving thread and the shutdown handle can both hold it
/// (`unblock` from the handle terminates the thread's `incoming_requests`).
pub struct BoundServer {
    server: Arc<tiny_http::Server>,
}

/// Live serving handle. [`ApiHandle::shutdown`] unblocks the serving thread and
/// joins it — dropping the thread's `ApiState` (and with it the `Arc<AppRuntime>`
/// clone) BEFORE the daemon drops the runtime, preserving the §8.2 audio
/// clock-release ordering (the API thread is one more `Arc<AppRuntime>` holder,
/// exactly like the driver and auth-retry tasks).
pub struct ApiHandle {
    server: Arc<tiny_http::Server>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BoundServer {
    /// Actual address, useful when binding an ephemeral port for a fixture.
    pub fn local_addr(&self) -> SocketAddr {
        self.server.server_addr().to_ip().expect("TCP listener")
    }
}

impl ApiHandle {
    pub fn shutdown(mut self) {
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Why a bind failed — `AddrInUse` is the case the boot step-5 diagnosis probes
/// (foreign qbzd vs another process); everything else is a generic fatal.
#[derive(Debug)]
pub enum BindError {
    AddrInUse(SocketAddr),
    Other(String),
}

/// Boot step 5 (01 §8.1): bind only — stateless, so the foreign-occupant
/// diagnosis (in `daemon.rs`) runs BEFORE stores (6) and runtime composition (7).
pub fn bind(addr: SocketAddr) -> Result<BoundServer, BindError> {
    match tiny_http::Server::http(addr) {
        Ok(server) => Ok(BoundServer {
            server: Arc::new(server),
        }),
        Err(e) => Err(classify_bind_error(e, addr)),
    }
}

fn classify_bind_error(
    e: Box<dyn std::error::Error + Send + Sync + 'static>,
    addr: SocketAddr,
) -> BindError {
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        if io.kind() == std::io::ErrorKind::AddrInUse {
            return BindError::AddrInUse(addr);
        }
    }
    BindError::Other(e.to_string())
}

/// Boot step 11 (01 §8.1): start serving on the already-bound socket. Requests
/// are handled inline on one thread (serialized). `unblock` (from the handle)
/// ends `incoming_requests` for graceful shutdown.
pub fn serve(server: BoundServer, state: impl HttpHost) -> ApiHandle {
    log::info!("shared API serving");
    let srv = server.server;
    let srv_handle = srv.clone();
    let thread = std::thread::Builder::new()
        .name("qbz-control".into())
        .spawn(move || {
            for mut req in srv.incoming_requests() {
                // `/api/events` is a long-lived SSE stream: it would block this
                // single serving thread forever. Move it onto its OWN thread
                // (Request is Send) so the control plane keeps answering. The
                // origin/token gate is applied first, identically to `route`.
                let is_events = *req.method() == Method::Get
                    && req.url().split('?').next() == Some("/api/events");
                if is_events {
                    let has_origin = req.headers().iter().any(|h| h.field.equiv("Origin"));
                    let auth = req
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Authorization"))
                        .map(|h| h.value.as_str().to_owned());
                    if let Some(reject) = access_gate(
                        has_origin,
                        "GET",
                        "/api/events",
                        auth.as_deref(),
                        state.token(),
                    ) {
                        let _ = req.respond(reject.response());
                        continue;
                    }
                    let rx = state.events().subscribe();
                    std::thread::Builder::new()
                        .name("qbz-control-sse".into())
                        .spawn(move || sse::stream(req, rx))
                        .ok();
                    continue;
                }
                let has_origin = req.headers().iter().any(|h| h.field.equiv("Origin"));
                let auth = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str());
                let path = req.url().split('?').next().unwrap_or("");
                let resp = if let Some(reject) =
                    access_gate(has_origin, req.method().as_str(), path, auth, state.token())
                {
                    reject.response()
                } else {
                    state.route(&mut req)
                };
                let _ = req.respond(resp);
            }
        })
        .expect("failed to spawn shared API thread");
    ApiHandle {
        server: srv_handle,
        thread: Some(thread),
    }
}
