// Shared API — the `GET /api/events` Server-Sent Events stream
// (CONSOLE ext). A live push feed of CoreEvents so a client (a plasmoid, a bar
// applet, `qbzd watch`) reacts to playback/queue/library changes without
// polling.
//
// Concurrency: the control-plane serve loop is single-threaded, so an open SSE
// stream would starve every other request. `serve()` therefore moves this onto
// its OWN thread (Request is Send); `stream` blocks there until the client
// disconnects (a write fails) or the bus closes. The rusqlite-free bus is a
// tokio broadcast, drained here with `blocking_recv()` from the plain thread.
//
// Wire format: one SSE frame per emitted event —
//   event: <CoreEvent type>\n
//   data: {"type":"…","data":{…}}\n\n
// The `data` line preserves the tagged CoreEvent JSON envelope, with
// LoggedIn projected to public identity only. A leading comment primes the
// stream; a dropped-lag notice reports lost events.
use qbz_models::CoreEvent;
use std::io::{self, Write};
use tiny_http::Request;
use tokio::sync::broadcast;

/// Send each frame immediately. tiny_http's Response reader path uses a
/// chunked-transfer encoder with an 8 KiB buffer and cannot flush between
/// events. Taking the response writer keeps its connection lifecycle while
/// letting us flush complete HTTP chunks before blocking for the next event.
pub fn stream(req: Request, rx: broadcast::Receiver<CoreEvent>) {
    let chunked = (req.http_version().0, req.http_version().1) >= (1, 1);
    let mut writer = req.into_writer();
    let _ = write_stream(&mut writer, rx, chunked);
}

fn write_stream(
    writer: &mut dyn Write,
    rx: broadcast::Receiver<CoreEvent>,
    chunked: bool,
) -> io::Result<()> {
    writer.write_all(if chunked {
        b"HTTP/1.1 200 OK\r\n"
    } else {
        b"HTTP/1.0 200 OK\r\n"
    })?;
    writer.write_all(b"Content-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nX-Accel-Buffering: no\r\n")?;
    if chunked {
        writer.write_all(b"Transfer-Encoding: chunked\r\n")?;
    }
    writer.write_all(b"\r\n")?;
    let mut events = EventFrames { rx, primed: false };
    while let Some(frame) = events.next_frame() {
        if chunked {
            write!(writer, "{:x}\r\n", frame.len())?;
        }
        writer.write_all(&frame)?;
        if chunked {
            writer.write_all(b"\r\n")?;
        }
        writer.flush()?;
    }
    if chunked {
        writer.write_all(b"0\r\n\r\n")?;
    }
    writer.flush()
}

struct EventFrames {
    rx: broadcast::Receiver<CoreEvent>,
    primed: bool,
}
impl EventFrames {
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        if !self.primed {
            self.primed = true;
            return Some(b": qbzd event stream\n\n".to_vec());
        }
        loop {
            match self.rx.blocking_recv() {
                Ok(ev) => {
                    if let Some(frame) = format_event(&ev) {
                        return Some(frame.into_bytes());
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    return Some(format!(": lagged {n} event(s)\n\n").into_bytes());
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Render one CoreEvent as an SSE frame, or `None` for events not worth pushing
/// to a UI client (bulky search payloads, internal loading/download/navigation
/// hints, diagnostics). Everything else — playback, queue, volume, auth,
/// favorites, playlists, errors, device changes — is emitted.
fn format_event(ev: &CoreEvent) -> Option<String> {
    if !emit(ev) {
        return None;
    }
    // Project from an allowlist instead of serializing credentials and trying
    // to strip known secret fields afterwards. New session fields stay private.
    let value = match ev {
        CoreEvent::LoggedIn { session } => serde_json::json!({
            "type": "LoggedIn",
            "data": {"session": {
                "user_id": session.user_id,
                "display_name": session.display_name,
            }},
        }),
        _ => serde_json::to_value(ev).ok()?,
    };
    let typ = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("event")
        .to_string();
    let data = serde_json::to_string(&value).ok()?;
    Some(format!("event: {typ}\ndata: {data}\n\n"))
}

fn emit(ev: &CoreEvent) -> bool {
    use CoreEvent::*;
    !matches!(
        ev,
        SearchResultsReceived { .. }
            | LoadingStarted { .. }
            | LoadingCompleted { .. }
            | DownloadProgress { .. }
            | DownloadCompleted { .. }
            | NavigateToAlbum { .. }
            | NavigateToArtist { .. }
            | NavigateToPlaylist { .. }
            | AudioDiagnostic { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use qbz_models::PlaybackState;

    #[test]
    fn stream_finishes_chunks_when_bus_closes_and_supports_http_10() {
        for chunked in [true, false] {
            let (tx, rx) = broadcast::channel(4);
            tx.send(CoreEvent::VolumeChanged { volume: 0.25 }).unwrap();
            drop(tx);
            let mut output = Vec::new();
            write_stream(&mut output, rx, chunked).unwrap();
            let wire = String::from_utf8(output).unwrap();
            assert!(wire.contains("event: VolumeChanged"));
            if chunked {
                assert!(wire.contains("Transfer-Encoding: chunked"));
                assert!(wire.ends_with("0\r\n\r\n"));
            } else {
                assert!(wire.starts_with("HTTP/1.0 200 OK"));
                assert!(!wire.contains("Transfer-Encoding"));
                assert!(wire.ends_with("\n\n"));
            }
        }
    }

    #[test]
    fn logged_in_frame_contains_public_identity_only() {
        let frame = format_event(&CoreEvent::LoggedIn {
            session: qbz_models::UserSession {
                user_auth_token: "fixture-private-token".into(),
                user_id: 7,
                display_name: "Listener".into(),
                email: "private@example.invalid".into(),
                ..Default::default()
            },
        })
        .unwrap();
        assert!(!frame.contains("fixture-private-token"));
        assert!(!frame.contains("user_auth_token"));
        assert!(!frame.contains("private@example.invalid"));
        let line = frame
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "LoggedIn",
                "data": {"session": {"user_id": 7, "display_name": "Listener"}}
            })
        );
    }

    #[test]
    fn playback_event_becomes_a_typed_sse_frame() {
        let frame = format_event(&CoreEvent::PlaybackStateChanged {
            state: PlaybackState::Playing,
        })
        .expect("playback event is emitted");
        assert!(frame.starts_with("event: PlaybackStateChanged\n"));
        assert!(frame.contains("data: {"));
        assert!(frame.ends_with("\n\n"));
        // The data line carries the tagged CoreEvent JSON.
        assert!(frame.contains("\"type\":\"PlaybackStateChanged\""));
    }

    #[test]
    fn bulky_and_internal_events_are_not_emitted() {
        assert!(format_event(&CoreEvent::LoadingStarted {
            operation: "x".into()
        })
        .is_none());
        assert!(format_event(&CoreEvent::DownloadCompleted { track_id: 1 }).is_none());
        assert!(format_event(&CoreEvent::NavigateToArtist { artist_id: 1 }).is_none());
    }

    #[test]
    fn volume_and_queue_events_are_emitted() {
        assert!(format_event(&CoreEvent::VolumeChanged { volume: 0.5 }).is_some());
        assert!(format_event(&CoreEvent::ShuffleChanged { enabled: true }).is_some());
    }
}
