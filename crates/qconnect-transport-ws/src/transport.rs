use std::{
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use prost::{
    encoding::{decode_varint, encode_varint},
    Message,
};
use qconnect_protocol::{
    decode_inbound_json, decode_queue_server_events, decode_renderer_server_commands,
    encode_outbound_payload_bytes, InboundEnvelope, OutboundEnvelope, QueueEventType,
    QueueServerEvent, RendererCommandType, RendererServerCommand,
};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc, watch, Mutex},
    task::JoinHandle,
};
use tokio_tungstenite::{
    client_async_tls_with_config,
    tungstenite::{
        handshake::client::Response, protocol::WebSocketConfig, Bytes, Message as WsMessage,
    },
    MaybeTlsStream, WebSocketStream,
};
use zeroize::{Zeroize, Zeroizing};

use crate::{WsTransportConfig, WsTransportError};

const MSG_TYPE_AUTHENTICATE: u8 = 1;
const MSG_TYPE_SUBSCRIBE: u8 = 2;
const MSG_TYPE_PAYLOAD: u8 = 6;
const MSG_TYPE_ERROR: u8 = 9;
const MSG_TYPE_DISCONNECT: u8 = 10;
const SHUTDOWN_CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransportEvent {
    Connected,
    Disconnected,
    Authenticated,
    Subscribed,
    /// Emitted on the first session-level acceptance proof for the active WS:
    /// controller sessions receive `SRVR_CTRL_SESSION_STATE`; delegated
    /// renderer handoffs receive targeted `SRVR_RNDR_SET_ACTIVE {active:true}`.
    /// A WS upgrade alone is never sufficient proof.
    SessionEstablished,
    /// Emitted when the reconnect loop has exhausted
    /// `reconnect_max_attempts` consecutive attempts without ever reaching
    /// `SessionEstablished`. The transport gives up after this event so the
    /// upper layer can decide whether to surface it to the user.
    MaxReconnectAttemptsExceeded {
        attempts: u32,
        last_reason: String,
    },
    ReconnectScheduled {
        attempt: u32,
        backoff_ms: u64,
        reason: String,
    },
    KeepalivePingSent,
    KeepalivePongReceived,
    TransportError {
        stage: String,
        message: String,
    },
    InboundFrameDecoded {
        cloud_message_type: u8,
        payload_size: usize,
    },
    InboundPayloadBytes {
        cloud_message_type: u8,
        payload: Vec<u8>,
    },
    OutboundSent {
        message_type: String,
        action_uuid: String,
    },
    /// Decoded `MSG_TYPE_ERROR` (cloud_type=9) frame from Qobuz cloud, per the
    /// `qws.proto` `ErrorMessage` definition. Emitted whenever the qws frontend
    /// rejects a session (e.g. zombie session, conflicting device, expired
    /// JWT). Only numeric protocol metadata crosses this boundary: `descr` is
    /// server-controlled and may reflect credentials, endpoints, account data,
    /// or session identifiers, so it is zeroized at decode time.
    CloudError { msg_id: u32, code: u32 },
    InboundQueueServerEvent(QueueServerEvent),
    InboundRendererServerCommand(RendererServerCommand),
    InboundReceived(InboundEnvelope),
}

#[async_trait]
pub trait WsTransport: Send + Sync {
    async fn connect(&self, config: WsTransportConfig) -> Result<(), WsTransportError>;
    async fn disconnect(&self) -> Result<(), WsTransportError>;
    async fn send(&self, envelope: OutboundEnvelope) -> Result<(), WsTransportError>;
    fn subscribe(&self) -> broadcast::Receiver<TransportEvent>;
}

#[derive(Debug, Default)]
struct InMemoryState {
    connected: bool,
    sent_messages: Vec<OutboundEnvelope>,
}

#[derive(Clone)]
pub struct InMemoryWsTransport {
    state: Arc<Mutex<InMemoryState>>,
    events_tx: broadcast::Sender<TransportEvent>,
}

impl InMemoryWsTransport {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(512);
        Self {
            state: Arc::new(Mutex::new(InMemoryState::default())),
            events_tx,
        }
    }

    pub async fn inject_inbound(&self, envelope: InboundEnvelope) -> Result<(), WsTransportError> {
        self.events_tx
            .send(TransportEvent::InboundReceived(envelope))
            .map_err(|_| WsTransportError::EventChannelClosed)?;
        Ok(())
    }

    pub async fn sent_messages(&self) -> Vec<OutboundEnvelope> {
        self.state.lock().await.sent_messages.clone()
    }

    pub async fn is_connected(&self) -> bool {
        self.state.lock().await.connected
    }
}

impl Default for InMemoryWsTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WsTransport for InMemoryWsTransport {
    async fn connect(&self, config: WsTransportConfig) -> Result<(), WsTransportError> {
        let mut state = self.state.lock().await;
        if state.connected {
            return Err(WsTransportError::AlreadyConnected);
        }

        state.connected = true;
        drop(state);

        // The in-memory transport never uses credentials. Dropping the owned
        // config here invokes its zeroizing cleanup instead of retaining a
        // needless copy for the lifetime of the transport.
        drop(config);

        self.events_tx
            .send(TransportEvent::Connected)
            .map_err(|_| WsTransportError::EventChannelClosed)?;
        Ok(())
    }

    async fn disconnect(&self) -> Result<(), WsTransportError> {
        let mut state = self.state.lock().await;
        if !state.connected {
            return Err(WsTransportError::NotConnected);
        }

        state.connected = false;
        drop(state);

        self.events_tx
            .send(TransportEvent::Disconnected)
            .map_err(|_| WsTransportError::EventChannelClosed)?;
        Ok(())
    }

    async fn send(&self, envelope: OutboundEnvelope) -> Result<(), WsTransportError> {
        let mut state = self.state.lock().await;
        if !state.connected {
            return Err(WsTransportError::NotConnected);
        }

        state.sent_messages.push(envelope.clone());
        drop(state);

        self.events_tx
            .send(TransportEvent::OutboundSent {
                message_type: envelope.message_type,
                action_uuid: envelope.action_uuid,
            })
            .map_err(|_| WsTransportError::EventChannelClosed)?;
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<TransportEvent> {
        self.events_tx.subscribe()
    }
}

#[derive(Debug, Default)]
struct NativeState {
    connected: bool,
    running: bool,
}

struct NativeRuntime {
    outbound_tx: mpsc::Sender<OutboundEnvelope>,
    shutdown_tx: watch::Sender<bool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for NativeRuntime {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };

        // A dropped JoinHandle normally detaches its task. Explicitly request
        // shutdown and abort as a fallback so the task-owned config (and its
        // delegated JWT) cannot remain retained after the transport is gone.
        let _ = self.shutdown_tx.send(true);
        handle.abort();
    }
}

#[derive(Clone)]
pub struct NativeWsTransport {
    state: Arc<Mutex<NativeState>>,
    runtime: Arc<Mutex<Option<NativeRuntime>>>,
    events_tx: broadcast::Sender<TransportEvent>,
}

impl NativeWsTransport {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(2048);
        Self {
            state: Arc::new(Mutex::new(NativeState::default())),
            runtime: Arc::new(Mutex::new(None)),
            events_tx,
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.state.lock().await.connected
    }
}

impl Default for NativeWsTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WsTransport for NativeWsTransport {
    async fn connect(&self, config: WsTransportConfig) -> Result<(), WsTransportError> {
        let mut runtime_guard = self.runtime.lock().await;
        if runtime_guard.is_some() {
            return Err(WsTransportError::AlreadyRunning);
        }

        {
            let mut state = self.state.lock().await;
            state.running = true;
        }

        let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundEnvelope>(512);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let events_tx = self.events_tx.clone();
        let state = Arc::clone(&self.state);
        let handle = tokio::spawn(async move {
            run_native_transport_loop(config, outbound_rx, shutdown_rx, events_tx, state).await;
        });

        *runtime_guard = Some(NativeRuntime {
            outbound_tx,
            shutdown_tx,
            handle: Some(handle),
        });
        Ok(())
    }

    async fn disconnect(&self) -> Result<(), WsTransportError> {
        let mut runtime = self
            .runtime
            .lock()
            .await
            .take()
            .ok_or(WsTransportError::NotConnected)?;

        let _ = runtime.shutdown_tx.send(true);

        runtime
            .handle
            .take()
            .ok_or_else(|| {
                WsTransportError::Internal("native transport task handle missing".to_string())
            })?
            .await
            .map_err(|err| WsTransportError::Join(err.to_string()))?;

        let mut state = self.state.lock().await;
        state.connected = false;
        state.running = false;
        Ok(())
    }

    async fn send(&self, envelope: OutboundEnvelope) -> Result<(), WsTransportError> {
        let outbound_tx = self
            .runtime
            .lock()
            .await
            .as_ref()
            .map(|runtime| runtime.outbound_tx.clone())
            .ok_or(WsTransportError::NotConnected)?;

        outbound_tx
            .send(envelope)
            .await
            .map_err(|_| WsTransportError::TransportChannelClosed)
    }

    fn subscribe(&self) -> broadcast::Receiver<TransportEvent> {
        self.events_tx.subscribe()
    }
}

/// Dial `endpoint_url`, tunneled through the process-wide configured proxy
/// (`qbz_net_proxy::current()`) when there is one, direct otherwise. Returns
/// the same shape `connect_async_with_config` did, so every match arm at the
/// call site is unchanged — only the stream type inside `WebSocketStream`
/// changed, and the reader/writer helpers below are already generic over it.
async fn dial_ws(
    endpoint_url: &str,
) -> Result<
    (
        WebSocketStream<MaybeTlsStream<Pin<Box<dyn qbz_net_proxy::TunnelStream>>>>,
        Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    let io_err =
        |msg: String| tokio_tungstenite::tungstenite::Error::Io(std::io::Error::other(msg));

    let parsed = url::Url::parse(endpoint_url).map_err(|e| io_err(e.to_string()))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| io_err("endpoint URL has no host".to_string()))?
        .to_string();
    let port = parsed
        .port_or_known_default()
        .unwrap_or(match parsed.scheme() {
            "wss" => 443,
            _ => 80,
        });

    let stream: Pin<Box<dyn qbz_net_proxy::TunnelStream>> = match qbz_net_proxy::current() {
        Some(proxy) => qbz_net_proxy::connect_tunnel(&proxy, &host, port)
            .await
            .map_err(|e| {
                log::warn!("[QConnect/Transport] proxy tunnel failed: {e}");
                io_err(e.to_string())
            })?,
        None => {
            let tcp = TcpStream::connect((host.as_str(), port))
                .await
                .map_err(tokio_tungstenite::tungstenite::Error::Io)?;
            Box::pin(tcp)
        }
    };

    client_async_tls_with_config(endpoint_url, stream, Some(WebSocketConfig::default()), None).await
}

async fn run_native_transport_loop(
    config: WsTransportConfig,
    mut outbound_rx: mpsc::Receiver<OutboundEnvelope>,
    mut shutdown_rx: watch::Receiver<bool>,
    events_tx: broadcast::Sender<TransportEvent>,
    state: Arc<Mutex<NativeState>>,
) {
    let base_backoff = config.reconnect_backoff_ms.max(200);
    let max_backoff = config.reconnect_backoff_max_ms.max(base_backoff);
    let max_attempts = config.reconnect_max_attempts;
    let mut backoff = base_backoff;
    let mut reconnect_attempt: u32 = 0;
    let mut msg_id: u32 = 0;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let connect_result = tokio::time::timeout(
            Duration::from_millis(config.connect_timeout_ms.max(1000)),
            dial_ws(&config.endpoint_url),
        )
        .await;

        let (mut ws, _) = match connect_result {
            Ok(Ok((ws, response))) => (ws, response),
            Ok(Err(_)) => {
                let reason = "connect_error".to_string();
                match handle_reconnect_delay(
                    &events_tx,
                    &mut shutdown_rx,
                    &mut reconnect_attempt,
                    &mut backoff,
                    max_backoff,
                    max_attempts,
                    reason,
                )
                .await
                {
                    ReconnectOutcome::Continue => continue,
                    ReconnectOutcome::Shutdown => break,
                    ReconnectOutcome::Exhausted => {
                        if idle_retry_after_exhausted(
                            &mut shutdown_rx,
                            &mut reconnect_attempt,
                            &mut backoff,
                            base_backoff,
                            config.reconnect_idle_retry_ms,
                        )
                        .await
                        {
                            continue;
                        }
                        break;
                    }
                }
            }
            Err(_) => {
                match handle_reconnect_delay(
                    &events_tx,
                    &mut shutdown_rx,
                    &mut reconnect_attempt,
                    &mut backoff,
                    max_backoff,
                    max_attempts,
                    "connect_timeout".to_string(),
                )
                .await
                {
                    ReconnectOutcome::Continue => continue,
                    ReconnectOutcome::Shutdown => break,
                    ReconnectOutcome::Exhausted => {
                        if idle_retry_after_exhausted(
                            &mut shutdown_rx,
                            &mut reconnect_attempt,
                            &mut backoff,
                            base_backoff,
                            config.reconnect_idle_retry_ms,
                        )
                        .await
                        {
                            continue;
                        }
                        break;
                    }
                }
            }
        };

        // NOTE: The reconnect counter is intentionally NOT reset here. Reaching
        // a connected WS is not proof that the session-level join succeeded —
        // Qobuz cloud will accept the WS upgrade and then immediately emit
        // MSG_TYPE_ERROR after the JOIN_SESSION exchange (issue #358). The
        // counter resets only when we observe a `SessionEstablished` event
        // (see the inbound branch below).

        {
            let mut guard = state.lock().await;
            guard.connected = true;
        }
        emit(&events_tx, TransportEvent::Connected);

        if let Some(jwt_qws) = config.jwt_qws.as_ref() {
            if send_authenticate(&mut ws, &mut msg_id, jwt_qws)
                .await
                .is_err()
            {
                emit(
                    &events_tx,
                    TransportEvent::TransportError {
                        stage: "authenticate".to_string(),
                        message: "authenticate_failed".to_string(),
                    },
                );
                let _ = ws.close(None).await;
                {
                    let mut guard = state.lock().await;
                    guard.connected = false;
                }
                emit(&events_tx, TransportEvent::Disconnected);
                match handle_reconnect_delay(
                    &events_tx,
                    &mut shutdown_rx,
                    &mut reconnect_attempt,
                    &mut backoff,
                    max_backoff,
                    max_attempts,
                    "authenticate_failed".to_string(),
                )
                .await
                {
                    ReconnectOutcome::Continue => continue,
                    ReconnectOutcome::Shutdown => break,
                    ReconnectOutcome::Exhausted => {
                        if idle_retry_after_exhausted(
                            &mut shutdown_rx,
                            &mut reconnect_attempt,
                            &mut backoff,
                            base_backoff,
                            config.reconnect_idle_retry_ms,
                        )
                        .await
                        {
                            continue;
                        }
                        break;
                    }
                }
            }
            emit(&events_tx, TransportEvent::Authenticated);
        } else if config.require_jwt {
            // gap #12: the real Qobuz endpoint requires AUTHENTICATE. Silently
            // skipping it leaves the session un-joinable; surface it as a hard
            // credential error and route it through the existing bounded
            // reconnect path (increment-only — issue #358 latch unaffected).
            emit(
                &events_tx,
                TransportEvent::TransportError {
                    stage: "authenticate".to_string(),
                    message: "missing jwt_qws for endpoint that requires AUTHENTICATE".to_string(),
                },
            );
            let _ = ws.close(None).await;
            {
                let mut guard = state.lock().await;
                guard.connected = false;
            }
            emit(&events_tx, TransportEvent::Disconnected);
            match handle_reconnect_delay(
                &events_tx,
                &mut shutdown_rx,
                &mut reconnect_attempt,
                &mut backoff,
                max_backoff,
                max_attempts,
                "missing_authenticate_jwt".to_string(),
            )
            .await
            {
                ReconnectOutcome::Continue => continue,
                ReconnectOutcome::Shutdown => break,
                ReconnectOutcome::Exhausted => {
                    if idle_retry_after_exhausted(
                        &mut shutdown_rx,
                        &mut reconnect_attempt,
                        &mut backoff,
                        base_backoff,
                        config.reconnect_idle_retry_ms,
                    )
                    .await
                    {
                        continue;
                    }
                    break;
                }
            }
        }

        if config.auto_subscribe {
            if send_subscribe(
                &mut ws,
                &mut msg_id,
                config.qcloud_proto,
                &config.subscribe_channels,
            )
            .await
            .is_err()
            {
                emit(
                    &events_tx,
                    TransportEvent::TransportError {
                        stage: "subscribe".to_string(),
                        message: "subscribe_failed".to_string(),
                    },
                );
                let _ = ws.close(None).await;
                {
                    let mut guard = state.lock().await;
                    guard.connected = false;
                }
                emit(&events_tx, TransportEvent::Disconnected);
                match handle_reconnect_delay(
                    &events_tx,
                    &mut shutdown_rx,
                    &mut reconnect_attempt,
                    &mut backoff,
                    max_backoff,
                    max_attempts,
                    "subscribe_failed".to_string(),
                )
                .await
                {
                    ReconnectOutcome::Continue => continue,
                    ReconnectOutcome::Shutdown => break,
                    ReconnectOutcome::Exhausted => {
                        if idle_retry_after_exhausted(
                            &mut shutdown_rx,
                            &mut reconnect_attempt,
                            &mut backoff,
                            base_backoff,
                            config.reconnect_idle_retry_ms,
                        )
                        .await
                        {
                            continue;
                        }
                        break;
                    }
                }
            }
            emit(&events_tx, TransportEvent::Subscribed);
        }

        let mut keepalive = tokio::time::interval(Duration::from_millis(
            config.keepalive_interval_ms.max(1_000),
        ));
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Per-connection latch. Only the first controller SESSION_STATE or
        // delegated renderer SET_ACTIVE=true resets the reconnect counters.
        let mut session_established = false;

        // Per-connection keepalive half-open tracking (gap #6). The deadline is
        // ~2.5× the ping interval: a healthy peer answers within one interval,
        // so two unanswered pings plus 2.5× silence is a confident half-open
        // signal without being trigger-happy.
        let keepalive_deadline_ms = config.keepalive_interval_ms.max(1_000).saturating_mul(5) / 2;
        let mut last_pong_at_ms = now_ms();
        let mut outstanding_pings: u32 = 0;

        let disconnect_reason = loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        let _ = tokio::time::timeout(
                            SHUTDOWN_CLOSE_TIMEOUT,
                            ws.close(None),
                        )
                        .await;
                        break "shutdown".to_string();
                    }
                }
                maybe_envelope = outbound_rx.recv() => {
                    match maybe_envelope {
                        Some(envelope) => {
                            if send_outbound_payload(
                                &mut ws,
                                &mut msg_id,
                                config.qcloud_proto,
                                &envelope,
                            ).await.is_err() {
                                break "send_error".to_string();
                            }
                            emit(
                                &events_tx,
                                TransportEvent::OutboundSent {
                                    message_type: envelope.message_type,
                                    action_uuid: envelope.action_uuid,
                                },
                            );
                        }
                        None => break "outbound_channel_closed".to_string(),
                    }
                }
                _ = keepalive.tick() => {
                    // Detect a half-open connection BEFORE sending the next ping:
                    // if prior pings went unanswered past the deadline, the TCP
                    // path is dead even though no read/write has errored yet.
                    // Breaking falls into the existing increment-only reconnect
                    // path (issue #358 latch unaffected).
                    if keepalive_is_stale(
                        now_ms(),
                        last_pong_at_ms,
                        outstanding_pings,
                        keepalive_deadline_ms,
                    ) {
                        break "keepalive_timeout".to_string();
                    }
                    if ws.send(WsMessage::Ping(Vec::new().into())).await.is_err() {
                        break "keepalive_ping_error".to_string();
                    }
                    outstanding_pings = outstanding_pings.saturating_add(1);
                    emit(&events_tx, TransportEvent::KeepalivePingSent);
                }
                incoming = ws.next() => {
                    match incoming {
                        Some(Ok(WsMessage::Binary(data))) => {
                            match handle_incoming_binary(
                                &events_tx,
                                &data,
                                config.jwt_qws.as_deref(),
                                Some(&config.endpoint_url),
                            ) {
                                Ok(InboundFrameOutcome { session_established_seen }) => {
                                    if session_established_seen && !session_established {
                                        session_established = true;
                                        reconnect_attempt = 0;
                                        backoff = base_backoff;
                                        emit(&events_tx, TransportEvent::SessionEstablished);
                                    }
                                }
                                Err(err) => {
                                    emit(
                                        &events_tx,
                                        TransportEvent::TransportError {
                                            stage: "decode_inbound_binary".to_string(),
                                            message: redact_sensitive(
                                                err.to_string(),
                                                config.jwt_qws.as_deref(),
                                                Some(&config.endpoint_url),
                                            ),
                                        },
                                    );
                                }
                            }
                        }
                        Some(Ok(WsMessage::Pong(_))) => {
                            last_pong_at_ms = now_ms();
                            outstanding_pings = 0;
                            emit(&events_tx, TransportEvent::KeepalivePongReceived);
                        }
                        Some(Ok(WsMessage::Ping(payload))) => {
                            let _ = ws.send(WsMessage::Pong(payload)).await;
                        }
                        Some(Ok(WsMessage::Close(_))) => {
                            break "remote_close".to_string();
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) => {
                            break "ws_read_error".to_string();
                        }
                        None => {
                            break "ws_stream_end".to_string();
                        }
                    }
                }
            }
        };

        {
            let mut guard = state.lock().await;
            guard.connected = false;
        }
        emit(&events_tx, TransportEvent::Disconnected);

        if *shutdown_rx.borrow() {
            break;
        }

        let disconnect_reason = redact_sensitive(
            disconnect_reason,
            config.jwt_qws.as_deref(),
            Some(&config.endpoint_url),
        );
        match handle_reconnect_delay(
            &events_tx,
            &mut shutdown_rx,
            &mut reconnect_attempt,
            &mut backoff,
            max_backoff,
            max_attempts,
            disconnect_reason,
        )
        .await
        {
            ReconnectOutcome::Continue => continue,
            ReconnectOutcome::Shutdown => break,
            ReconnectOutcome::Exhausted => {
                if idle_retry_after_exhausted(
                    &mut shutdown_rx,
                    &mut reconnect_attempt,
                    &mut backoff,
                    base_backoff,
                    config.reconnect_idle_retry_ms,
                )
                .await
                {
                    continue;
                }
                break;
            }
        }
    }

    let mut guard = state.lock().await;
    guard.connected = false;
    guard.running = false;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconnectOutcome {
    /// Backoff completed; the loop should attempt to reconnect.
    Continue,
    /// Shutdown was requested while waiting; the loop must terminate.
    Shutdown,
    /// `reconnect_max_attempts` was exceeded without ever reaching session
    /// establishment; the loop must terminate and surface the failure.
    Exhausted,
}

async fn handle_reconnect_delay(
    events_tx: &broadcast::Sender<TransportEvent>,
    shutdown_rx: &mut watch::Receiver<bool>,
    reconnect_attempt: &mut u32,
    backoff_ms: &mut u64,
    max_backoff: u64,
    max_attempts: Option<u32>,
    reason: String,
) -> ReconnectOutcome {
    *reconnect_attempt = reconnect_attempt.saturating_add(1);

    if let Some(cap) = max_attempts {
        if cap > 0 && *reconnect_attempt > cap {
            emit(
                events_tx,
                TransportEvent::MaxReconnectAttemptsExceeded {
                    attempts: *reconnect_attempt,
                    last_reason: reason,
                },
            );
            return ReconnectOutcome::Exhausted;
        }
    }

    emit(
        events_tx,
        TransportEvent::ReconnectScheduled {
            attempt: *reconnect_attempt,
            backoff_ms: *backoff_ms,
            reason,
        },
    );

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(*backoff_ms)) => {}
        _ = shutdown_rx.changed() => {
            if *shutdown_rx.borrow() {
                return ReconnectOutcome::Shutdown;
            }
        }
    }

    *backoff_ms = (*backoff_ms).saturating_mul(2).min(max_backoff);
    ReconnectOutcome::Continue
}

/// Long-backoff idle retry after the reconnect loop has Exhausted its bounded
/// attempts (gap #7). When `idle_retry_ms == 0` this is a no-op returning
/// `false` (legacy terminate behavior). Otherwise it sleeps `idle_retry_ms`
/// (cancellable by shutdown); on a real shutdown it returns `false` without
/// re-arming. On a clean wake it resets the attempt counter / backoff to base
/// and returns `true` so the caller can `continue` the loop.
///
/// NOTE: this resets the counters ONLY after Exhausted + a deliberate idle
/// wait — never on connect. The session-acceptance latch is therefore
/// untouched.
async fn idle_retry_after_exhausted(
    shutdown_rx: &mut watch::Receiver<bool>,
    reconnect_attempt: &mut u32,
    backoff: &mut u64,
    base_backoff: u64,
    idle_retry_ms: u64,
) -> bool {
    if idle_retry_ms == 0 {
        return false;
    }
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(idle_retry_ms)) => {}
        _ = shutdown_rx.changed() => {
            if *shutdown_rx.borrow() {
                return false;
            }
        }
    }
    *reconnect_attempt = 0;
    *backoff = base_backoff;
    true
}

/// Side-channel result from `handle_incoming_binary`: the transport owns the
/// per-connection reconnect latch, so acceptance is recognized before decoded
/// events can be delayed in downstream broadcast consumers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct InboundFrameOutcome {
    session_established_seen: bool,
}

fn is_session_established_command(command: &RendererServerCommand) -> bool {
    command.command_type == RendererCommandType::SrvrRndrSetActive
        && command
            .payload
            .get("active")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

fn handle_incoming_binary(
    events_tx: &broadcast::Sender<TransportEvent>,
    data: &[u8],
    jwt_qws: Option<&str>,
    sensitive_endpoint: Option<&str>,
) -> Result<InboundFrameOutcome, WsTransportError> {
    let (cloud_message_type, payload) = decode_qcloud_frame(data)?;
    let mut outcome = InboundFrameOutcome::default();

    emit(
        events_tx,
        TransportEvent::InboundFrameDecoded {
            cloud_message_type,
            payload_size: payload.len(),
        },
    );

    match cloud_message_type {
        MSG_TYPE_PAYLOAD => {
            let cloud_payload = CloudPayload::decode(payload).map_err(|err| {
                WsTransportError::Protocol(format!("decode payload envelope: {err}"))
            })?;

            let inner_payload = cloud_payload.payload.unwrap_or_default();
            emit(
                events_tx,
                TransportEvent::InboundPayloadBytes {
                    cloud_message_type,
                    payload: inner_payload.clone(),
                },
            );

            // inner_payload is a QConnectBatch (QConnectMessages) protobuf:
            //   field 1 (fixed64): messages_time
            //   field 2 (int32): messages_id
            //   field 3 (repeated): QConnectMessage entries
            // Pass directly to decoders — no inner envelope unwrapping needed.
            log::debug!(
                "[QConnect/Decode] Decoding QConnectBatch: {} bytes",
                inner_payload.len()
            );

            match decode_queue_server_events(&inner_payload) {
                Ok(events) => {
                    log::debug!("[QConnect/Decode] Queue events decoded: {}", events.len());
                    for event in events {
                        if event.event_type == QueueEventType::SrvrCtrlSessionState {
                            outcome.session_established_seen = true;
                        }
                        emit(events_tx, TransportEvent::InboundQueueServerEvent(event));
                    }
                }
                Err(err) => {
                    let message = redact_sensitive(err.to_string(), jwt_qws, sensitive_endpoint);
                    log::warn!("[QConnect/Decode] Queue events decode error: {}", message);
                }
            }

            match decode_renderer_server_commands(&inner_payload) {
                Ok(commands) => {
                    if !commands.is_empty() {
                        log::debug!(
                            "[QConnect/Decode] Renderer commands decoded: {}",
                            commands.len()
                        );
                    }
                    for command in commands {
                        if is_session_established_command(&command) {
                            outcome.session_established_seen = true;
                        }
                        emit(
                            events_tx,
                            TransportEvent::InboundRendererServerCommand(command),
                        );
                    }
                }
                Err(err) => {
                    let message = redact_sensitive(err.to_string(), jwt_qws, sensitive_endpoint);
                    log::debug!(
                        "[QConnect/Decode] Renderer commands decode error: {}",
                        message
                    );
                }
            }

            if let Ok(inbound) = decode_inbound_json(&inner_payload) {
                emit(events_tx, TransportEvent::InboundReceived(inbound));
            }
        }
        MSG_TYPE_ERROR => {
            // Decode the qws-level ErrorMessage (qws.proto messageTypeId=9):
            //   uint32 msg_id = 1;
            //   uint64 msg_date = 2;
            //   uint32 code = 3;
            //   string descr = 4;
            // This is the only signal we get for "the qws frontend rejected
            // your session" (zombie session, conflicting device, expired JWT,
            // ...). Without decoding it we cannot tell *why* the cloud is
            // tearing us down — see issue #358.
            match QwsErrorMessage::decode(payload) {
                Ok(error) => {
                    let msg_id = error.msg_id.unwrap_or(0);
                    let code = error.code.unwrap_or(0);
                    let mut descr = error.descr.unwrap_or_default();
                    descr.zeroize();
                    log::warn!(
                        "[QConnect/Transport] Cloud error frame: msg_id={} code={}",
                        msg_id, code
                    );
                    emit(events_tx, TransportEvent::CloudError { msg_id, code });
                }
                Err(err) => {
                    log::warn!(
                        "[QConnect/Transport] Cloud error frame failed to decode as qws.ErrorMessage: bytes={} err={}",
                        payload.len(),
                        err
                    );
                    emit(
                        events_tx,
                        TransportEvent::TransportError {
                            stage: "qcloud_error_frame_decode".to_string(),
                            message: format!("bytes={} err={}", payload.len(), err),
                        },
                    );
                }
            }
        }
        MSG_TYPE_DISCONNECT => {
            emit(
                events_tx,
                TransportEvent::TransportError {
                    stage: "qcloud_disconnect_frame".to_string(),
                    message: "remote_disconnect_signal".to_string(),
                },
            );
        }
        _ => {}
    }

    Ok(outcome)
}

async fn send_authenticate<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    msg_id: &mut u32,
    jwt_qws: &str,
) -> Result<(), WsTransportError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut auth = Authenticate {
        msg_id: Some(next_msg_id(msg_id)),
        msg_date: Some(now_ms()),
        jwt: Some(jwt_qws.to_string()),
    };

    // Encode directly into the qcloud frame so the protobuf payload is not a
    // second temporary buffer containing the JWT. The unavoidable protobuf
    // String copy is scrubbed before the first await point.
    let frame_result = encode_qcloud_message(MSG_TYPE_AUTHENTICATE, &auth);
    if let Some(jwt_qws) = auth.jwt.as_mut() {
        jwt_qws.zeroize();
    }
    // Keep the encoded frame in an owner that scrubs its allocation when the
    // last tungstenite `Bytes` reference is released.
    let frame = Bytes::from_owner(Zeroizing::new(frame_result?));

    ws.send(WsMessage::Binary(frame))
        .await
        .map_err(|err| WsTransportError::Protocol(format!("send authenticate: {err}")))
}

async fn send_subscribe<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    msg_id: &mut u32,
    qcloud_proto: u32,
    channels: &[Vec<u8>],
) -> Result<(), WsTransportError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let subscribe = Subscribe {
        msg_id: Some(next_msg_id(msg_id)),
        msg_date: Some(now_ms()),
        proto: Some(qcloud_proto),
        channels: channels.to_vec(),
    };

    let frame = encode_qcloud_frame(MSG_TYPE_SUBSCRIBE, &subscribe.encode_to_vec());
    ws.send(WsMessage::Binary(frame.into()))
        .await
        .map_err(|err| WsTransportError::Protocol(format!("send subscribe: {err}")))
}

async fn send_outbound_payload<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    msg_id: &mut u32,
    qcloud_proto: u32,
    envelope: &OutboundEnvelope,
) -> Result<(), WsTransportError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let payload_bytes = encode_outbound_payload_bytes(envelope)
        .map_err(|err| WsTransportError::Serialization(err.to_string()))?;

    let payload = CloudPayload {
        msg_id: Some(next_msg_id(msg_id)),
        msg_date: Some(now_ms()),
        proto: Some(qcloud_proto),
        src: None,
        dests: Vec::new(),
        payload: Some(payload_bytes),
    };

    let frame = encode_qcloud_frame(MSG_TYPE_PAYLOAD, &payload.encode_to_vec());
    ws.send(WsMessage::Binary(frame.into()))
        .await
        .map_err(|err| WsTransportError::Protocol(format!("send payload: {err}")))
}

fn encode_qcloud_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + 10 + payload.len());
    frame.push(msg_type);
    encode_varint(payload.len() as u64, &mut frame);
    frame.extend_from_slice(payload);
    frame
}

fn encode_qcloud_message<M: Message>(
    msg_type: u8,
    message: &M,
) -> Result<Vec<u8>, WsTransportError> {
    let payload_len = message.encoded_len();
    let mut frame = Vec::with_capacity(1 + 10 + payload_len);
    frame.push(msg_type);
    encode_varint(payload_len as u64, &mut frame);
    message
        .encode(&mut frame)
        .map_err(|err| WsTransportError::Serialization(err.to_string()))?;
    Ok(frame)
}

/// Remove delegated credentials and endpoint routing material from any
/// server-controlled diagnostic before it reaches logs or transport events.
/// Each replaced allocation is overwritten before the safe value continues.
fn redact_sensitive(
    mut message: String,
    jwt_qws: Option<&str>,
    sensitive_endpoint: Option<&str>,
) -> String {
    for sensitive in [jwt_qws, sensitive_endpoint]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
    {
        if message.contains(sensitive) {
            let redacted = message.replace(sensitive, "[REDACTED]");
            message.zeroize();
            message = redacted;
        }
    }
    message
}

fn decode_qcloud_frame(data: &[u8]) -> Result<(u8, &[u8]), WsTransportError> {
    if data.is_empty() {
        return Err(WsTransportError::Protocol("empty qcloud frame".to_string()));
    }

    let msg_type = data[0];
    let mut cursor = &data[1..];
    let payload_len = decode_varint(&mut cursor)
        .map_err(|err| WsTransportError::Protocol(format!("decode varint length: {err}")))?
        as usize;

    let consumed_varint = data.len().saturating_sub(1 + cursor.len());
    let payload_start = 1 + consumed_varint;

    if data.len() < payload_start + payload_len {
        return Err(WsTransportError::Protocol(format!(
            "truncated qcloud frame: expected={}, got={}",
            payload_start + payload_len,
            data.len()
        )));
    }

    Ok((msg_type, &data[payload_start..payload_start + payload_len]))
}

fn emit(events_tx: &broadcast::Sender<TransportEvent>, event: TransportEvent) {
    let _ = events_tx.send(event);
}

fn next_msg_id(msg_id: &mut u32) -> u32 {
    *msg_id = msg_id.saturating_add(1);
    *msg_id
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Pure half-open TCP detector (gap #6). Returns `true` only when at least two
/// keepalive pings have gone unanswered AND the last pong is older than
/// `max_silence_ms`. Requiring two outstanding pings avoids tearing down a
/// healthy connection that simply missed a single pong window.
fn keepalive_is_stale(
    now_ms: u64,
    last_pong_at_ms: u64,
    outstanding_pings: u32,
    max_silence_ms: u64,
) -> bool {
    outstanding_pings >= 2 && now_ms.saturating_sub(last_pong_at_ms) >= max_silence_ms
}

#[derive(Clone, PartialEq, ::prost::Message)]
struct Authenticate {
    #[prost(uint32, optional, tag = "1")]
    pub msg_id: Option<u32>,
    #[prost(uint64, optional, tag = "2")]
    pub msg_date: Option<u64>,
    #[prost(string, optional, tag = "3")]
    pub jwt: Option<String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
struct Subscribe {
    #[prost(uint32, optional, tag = "1")]
    pub msg_id: Option<u32>,
    #[prost(uint64, optional, tag = "2")]
    pub msg_date: Option<u64>,
    #[prost(uint32, optional, tag = "3")]
    pub proto: Option<u32>,
    #[prost(bytes = "vec", repeated, tag = "4")]
    pub channels: Vec<Vec<u8>>,
}

/// Mirror of `qws.proto` `ErrorMessage` (messageTypeId=9). Carried as the
/// payload of `MSG_TYPE_ERROR` qcloud frames. All fields are optional to
/// match proto3 semantics (see issue #358).
#[derive(Clone, PartialEq, ::prost::Message)]
struct QwsErrorMessage {
    #[prost(uint32, optional, tag = "1")]
    pub msg_id: Option<u32>,
    #[prost(uint64, optional, tag = "2")]
    pub msg_date: Option<u64>,
    #[prost(uint32, optional, tag = "3")]
    pub code: Option<u32>,
    #[prost(string, optional, tag = "4")]
    pub descr: Option<String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
struct CloudPayload {
    #[prost(uint32, optional, tag = "1")]
    pub msg_id: Option<u32>,
    #[prost(uint64, optional, tag = "2")]
    pub msg_date: Option<u64>,
    #[prost(uint32, optional, tag = "3")]
    pub proto: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub src: Option<Vec<u8>>,
    #[prost(bytes = "vec", repeated, tag = "5")]
    pub dests: Vec<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "7")]
    pub payload: Option<Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gap #12: `require_jwt` must default to `false` so the InMemory / test
    /// transport path keeps working without a JWT. Only the real Qobuz endpoint
    /// (set in `resolve_transport_config`) flips it on.
    #[test]
    fn config_require_jwt_defaults_false_for_test_path() {
        let cfg = WsTransportConfig::default();
        assert!(!cfg.require_jwt);
    }

    /// gap #6: half-open detection only fires after BOTH at least two
    /// unanswered pings AND silence past the deadline. A single missed pong, or
    /// recent silence with fewer than two outstanding pings, must NOT tear the
    /// connection down.
    #[test]
    fn keepalive_stale_only_after_two_unanswered_pings_and_silence() {
        let interval = 30_000u64;
        let deadline = interval * 5 / 2;
        let now = 1_000_000u64;
        assert!(!keepalive_is_stale(now, now, 3, deadline));
        assert!(!keepalive_is_stale(now, now - deadline - 1, 0, deadline));
        assert!(!keepalive_is_stale(now, now - deadline - 1, 1, deadline));
        assert!(keepalive_is_stale(now, now - deadline - 1, 2, deadline));
    }

    /// gap #7: with `idle_retry_ms == 0` the helper is a no-op — it returns
    /// `false` (terminate) and leaves the counters untouched.
    #[tokio::test]
    async fn idle_retry_disabled_when_interval_zero() {
        let (_tx, mut rx) = watch::channel(false);
        let mut attempt = 5u32;
        let mut backoff = 8u64;
        assert!(!idle_retry_after_exhausted(&mut rx, &mut attempt, &mut backoff, 2, 0).await);
        assert_eq!(attempt, 5);
    }

    /// gap #7: a shutdown signalled during the idle wait must cancel the retry
    /// and NOT re-arm (returns `false`, counters untouched).
    #[tokio::test]
    async fn idle_retry_cancelled_by_shutdown_does_not_rearm() {
        let (tx, mut rx) = watch::channel(false);
        let _ = tx.send(true);
        let mut attempt = 5u32;
        let mut backoff = 8u64;
        assert!(!idle_retry_after_exhausted(&mut rx, &mut attempt, &mut backoff, 2, 60_000).await);
    }

    #[test]
    fn qcloud_frame_roundtrip() {
        let payload = vec![0x01, 0x02, 0x03, 0x04];
        let encoded = encode_qcloud_frame(MSG_TYPE_PAYLOAD, &payload);
        let (msg_type, decoded_payload) = decode_qcloud_frame(&encoded).expect("decode frame");
        assert_eq!(msg_type, MSG_TYPE_PAYLOAD);
        assert_eq!(decoded_payload, payload.as_slice());
    }

    /// Issue #358: a qws-level `MSG_TYPE_ERROR` frame must surface as a
    /// `CloudError` event carrying the decoded numeric code, NOT as an opaque
    /// `bytes=N` `TransportError`. Without this, the upper layer can't
    /// distinguish a zombie session from any other transport hiccup and the
    /// reconnect loop spins blindly.
    #[tokio::test]
    async fn msg_type_error_frame_decodes_into_cloud_error_event() {
        let (events_tx, mut events_rx) = broadcast::channel::<TransportEvent>(8);

        let error_payload = QwsErrorMessage {
            msg_id: Some(42),
            msg_date: Some(1_700_000_000_000),
            code: Some(403),
            descr: Some("zombie_session".to_string()),
        }
        .encode_to_vec();
        let frame = encode_qcloud_frame(MSG_TYPE_ERROR, &error_payload);

        let outcome = handle_incoming_binary(&events_tx, &frame, None, None).expect("decode frame");
        assert!(!outcome.session_established_seen);

        // First event is always InboundFrameDecoded; skip it and assert the
        // semantic event afterwards is CloudError, not the legacy
        // TransportError.
        let first = events_rx.recv().await.expect("first event");
        assert!(matches!(first, TransportEvent::InboundFrameDecoded { .. }));

        match events_rx.recv().await.expect("second event") {
            TransportEvent::CloudError { msg_id, code } => {
                assert_eq!(msg_id, 42);
                assert_eq!(code, 403);
            }
            other => panic!("expected CloudError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cloud_error_discards_server_description() {
        const JWT: &str = "header.payload.secret-signature";
        let (events_tx, mut events_rx) = broadcast::channel::<TransportEvent>(8);

        let error_payload = QwsErrorMessage {
            msg_id: Some(7),
            msg_date: None,
            code: Some(403),
            descr: Some(format!("rejected credential {JWT}")),
        }
        .encode_to_vec();
        let frame = encode_qcloud_frame(MSG_TYPE_ERROR, &error_payload);

        handle_incoming_binary(&events_tx, &frame, Some(JWT), None).expect("decode frame");
        let _ = events_rx.recv().await.expect("frame event");

        let event = events_rx.recv().await.expect("cloud error event");
        let debug = format!("{event:?}");
        assert!(!debug.contains(JWT));
        assert!(matches!(
            event,
            TransportEvent::CloudError {
                msg_id: 7,
                code: 403
            }
        ));
    }

    #[test]
    fn delegated_set_active_true_is_session_acceptance_proof() {
        let active = RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetActive,
            payload: serde_json::json!({ "active": true }),
        };
        let inactive = RendererServerCommand {
            command_type: RendererCommandType::SrvrRndrSetActive,
            payload: serde_json::json!({ "active": false }),
        };

        assert!(is_session_established_command(&active));
        assert!(!is_session_established_command(&inactive));
    }

    #[test]
    fn diagnostics_redact_both_delegated_secrets() {
        const JWT: &str = "header.payload.secret-signature";
        const ENDPOINT: &str = "wss://qws.example.test/ws?route=secret";
        let message = format!("rejected {JWT} at {ENDPOINT}");

        let redacted = redact_sensitive(message, Some(JWT), Some(ENDPOINT));

        assert_eq!(redacted, "rejected [REDACTED] at [REDACTED]");
    }

    #[test]
    fn qcloud_frame_rejects_truncated_payload() {
        let payload = vec![0x0a, 0x0b, 0x0c];
        let mut encoded = encode_qcloud_frame(MSG_TYPE_PAYLOAD, &payload);
        encoded.truncate(encoded.len() - 1);
        let err = decode_qcloud_frame(&encoded).expect_err("expected truncation error");
        assert!(matches!(err, WsTransportError::Protocol(_)));
    }

    #[test]
    fn cloud_payload_proto_roundtrip() {
        let payload = CloudPayload {
            msg_id: Some(12),
            msg_date: Some(34),
            proto: Some(1),
            src: None,
            dests: vec![vec![1, 2, 3]],
            payload: Some(vec![9, 8, 7]),
        };
        let encoded = payload.encode_to_vec();
        let decoded = CloudPayload::decode(encoded.as_slice()).expect("decode cloud payload");
        assert_eq!(decoded.msg_id, Some(12));
        assert_eq!(decoded.proto, Some(1));
        assert_eq!(decoded.payload, Some(vec![9, 8, 7]));
    }

    /// Issue #358 regression: with `reconnect_max_attempts = Some(N)`, the
    /// reconnect helper must:
    ///   1. NOT decrement / reset the counter on successive attempts (it
    ///      can only be reset externally — by `SessionEstablished`),
    ///   2. emit `MaxReconnectAttemptsExceeded` and return
    ///      `ReconnectOutcome::Exhausted` once the counter goes past N.
    /// If the counter were reset elsewhere (e.g. on `Connected`), the loop
    /// could spin forever at base backoff against a host that always rejects
    /// the session-level join.
    #[tokio::test]
    async fn handle_reconnect_delay_caps_attempts_at_max() {
        let (events_tx, mut events_rx) = broadcast::channel::<TransportEvent>(64);
        let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);

        // Sub-millisecond backoffs keep this test fast despite the real sleeps.
        let mut attempt: u32 = 0;
        let mut backoff: u64 = 1;
        let max_backoff: u64 = 4;
        let max_attempts = Some(3u32);

        // First three attempts schedule a reconnect, ramping the backoff and
        // never resetting the counter.
        for expected_attempt in 1..=3u32 {
            let outcome = handle_reconnect_delay(
                &events_tx,
                &mut shutdown_rx,
                &mut attempt,
                &mut backoff,
                max_backoff,
                max_attempts,
                "test_reason".to_string(),
            )
            .await;
            assert_eq!(outcome, ReconnectOutcome::Continue);
            assert_eq!(attempt, expected_attempt);
        }

        // Drain the three ReconnectScheduled events, asserting the attempt
        // counter monotonically increases (i.e. is NOT reset between calls).
        for expected_attempt in 1..=3u32 {
            match events_rx.recv().await.expect("event") {
                TransportEvent::ReconnectScheduled { attempt, .. } => {
                    assert_eq!(attempt, expected_attempt);
                }
                other => panic!("expected ReconnectScheduled, got {other:?}"),
            }
        }

        // Fourth call exceeds the cap: counter goes to 4, helper emits
        // MaxReconnectAttemptsExceeded and returns Exhausted (no sleep, no
        // ReconnectScheduled).
        let outcome = handle_reconnect_delay(
            &events_tx,
            &mut shutdown_rx,
            &mut attempt,
            &mut backoff,
            max_backoff,
            max_attempts,
            "test_reason".to_string(),
        )
        .await;
        assert_eq!(outcome, ReconnectOutcome::Exhausted);
        assert_eq!(attempt, 4);

        match events_rx.recv().await.expect("event") {
            TransportEvent::MaxReconnectAttemptsExceeded {
                attempts,
                last_reason,
            } => {
                assert_eq!(attempts, 4);
                assert_eq!(last_reason, "test_reason");
            }
            other => panic!("expected MaxReconnectAttemptsExceeded, got {other:?}"),
        }
    }

    /// `reconnect_max_attempts = None` preserves the legacy unbounded
    /// behavior used by tests / non-Qobuz endpoints.
    #[tokio::test]
    async fn handle_reconnect_delay_unbounded_when_max_attempts_is_none() {
        let (events_tx, _events_rx) = broadcast::channel::<TransportEvent>(16);
        let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let mut attempt: u32 = 0;
        let mut backoff: u64 = 1;

        for _ in 0..5 {
            let outcome = handle_reconnect_delay(
                &events_tx,
                &mut shutdown_rx,
                &mut attempt,
                &mut backoff,
                4,
                None,
                "noop".to_string(),
            )
            .await;
            assert_eq!(outcome, ReconnectOutcome::Continue);
        }
        assert_eq!(attempt, 5);
    }

    // `qbz_net_proxy::current()`/`set_current()` are process-wide; serialize
    // the tests that touch it so they can't observe each other's writes.
    static PROXY_SERIAL: Mutex<()> = Mutex::const_new(());

    #[tokio::test]
    async fn dial_ws_tunnels_through_the_configured_proxy() {
        let _lock = PROXY_SERIAL.lock().await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
            let _ = tx.send(String::from_utf8_lossy(&buffer).into_owned());
            // No real WS server behind the tunnel: refuse it so dial_ws
            // returns quickly instead of hanging on a handshake that will
            // never complete.
            let _ = stream.shutdown().await;
        });

        qbz_net_proxy::set_current(Some(qbz_net_proxy::ProxyConfig {
            kind: qbz_net_proxy::ProxyKind::Http,
            host: "127.0.0.1".to_string(),
            port: proxy_port,
            auth: None,
        }));
        let _ = dial_ws("wss://example.invalid:443/ws").await;
        qbz_net_proxy::set_current(None);

        let request = rx.await.unwrap();
        assert!(request.starts_with("CONNECT example.invalid:443 HTTP/1.1"));
    }

    #[tokio::test]
    async fn dial_ws_connects_directly_with_no_proxy_configured() {
        let _lock = PROXY_SERIAL.lock().await;
        qbz_net_proxy::set_current(None);

        // Nothing listens on this port: a direct dial fails fast with a
        // connection-refused-shaped error rather than hanging, proving no
        // proxy tunnel was attempted (which would fail differently — "proxy
        // tunnel failed" in the log, not a bare connect error).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let result = dial_ws(&format!("ws://127.0.0.1:{port}/ws")).await;
        assert!(result.is_err());
    }
}
