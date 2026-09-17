//! Opt-in inspection host/client. A successful probe does NOT select a remote
//! playback context. Capabilities are checked before each laboratory operation.
use qbz_control::library::{LibraryHost, LibraryInfo, LibraryPage, PROTOCOL_VERSION};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| has_flag(std::env::args()))
}
fn has_flag(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|arg| arg == "--orbit")
}

struct Listener {
    handle: qbz_control::ApiHandle,
    address: String,
    token: String,
}
#[derive(Clone)]
struct Peer {
    url: reqwest::Url,
    token: String,
    info: LibraryInfo,
}
#[derive(Default)]
struct State {
    listener: Option<Listener>,
    peer: Option<Peer>,
    page: Option<LibraryPage>,
    host_generation: u64,
    peer_generation: u64,
    revision: u64,
    starting: bool,
    probing: bool,
    searching: bool,
    status: &'static str,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

pub fn publish() {
    if !enabled() {
        return;
    }
    let (revision, doc) = {
        let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        (
            state.revision,
            serde_json::json!({
                "listening": state.listener.is_some(), "starting": state.starting,
                "address": state.listener.as_ref().map(|s| s.address.as_str()),
                // Access key never enters JSON/QML. Copy requires a deliberate click.
                "peer": state.peer.as_ref().map(|p| &p.info),
                "page": state.page, "probing": state.probing, "searching": state.searching,
                "status": state.status,
            })
            .to_string(),
        )
    };
    crate::orbit_bridge::ui(move |mut bridge| {
        if STATE.lock().unwrap_or_else(|e| e.into_inner()).revision == revision {
            bridge
                .as_mut()
                .set_state_json(cxx_qt_lib::QString::from(doc));
        }
    });
}

pub fn start_host(address: String) {
    if !enabled() {
        return;
    }
    let Ok(address) = address.trim().parse::<std::net::SocketAddr>() else {
        set_status("invalid-address");
        return;
    };
    let Some(host) = crate::local_service_qt::current() else {
        set_status("host-unavailable");
        return;
    };
    let generation = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state.starting || state.listener.is_some() {
            return;
        }
        state.host_generation += 1;
        state.revision += 1;
        state.starting = true;
        state.status = "";
        state.host_generation
    };
    publish();
    crate::spawn(async move {
        let binding = host.clone();
        let result = tokio::task::spawn_blocking(move || {
            let bound = qbz_control::bind(address).map_err(|_| ())?;
            let address = bound.local_addr().to_string();
            let token = uuid::Uuid::new_v4().simple().to_string();
            qbz_log::register_secret(token.clone());
            let name = std::env::var("HOSTNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "QBZ".into());
            let endpoint = LibraryHost::new(host.service.clone(), name, token.clone());
            Ok::<_, ()>(Listener {
                handle: qbz_control::serve(bound, endpoint),
                address,
                token,
            })
        })
        .await;
        let mut discard = None;
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            let current = state.host_generation == generation
                && crate::local_service_qt::is_current(&binding);
            match result {
                Ok(Ok(listener)) if current => {
                    state.listener = Some(listener);
                }
                Ok(Ok(listener)) => discard = Some(listener),
                _ if current => state.status = "listen-failed",
                _ => {}
            }
            if current {
                state.starting = false;
                state.revision += 1;
            }
        }
        if let Some(listener) = discard {
            let _ = tokio::task::spawn_blocking(move || listener.handle.shutdown()).await;
        }
        publish();
    });
}

pub fn stop_host() {
    if !enabled() {
        return;
    }
    let listener = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.host_generation += 1;
        state.revision += 1;
        state.starting = false;
        state.listener.take()
    };
    if let Some(listener) = listener {
        crate::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || listener.handle.shutdown()).await;
        });
    }
    publish();
}

/// Session teardown also invalidates delayed client replies and drops its key.
pub fn reset() {
    if !enabled() {
        return;
    }
    stop_host();
    {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.peer_generation += 1;
        state.revision += 1;
        state.peer = None;
        state.page = None;
        state.probing = false;
        state.searching = false;
        state.status = "";
    }
    publish();
}

pub fn copy_access_key() {
    if !enabled() {
        return;
    }
    let token = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .listener
        .as_ref()
        .map(|s| s.token.clone());
    if let Some(token) = token {
        crate::share_qt::copy_to_clipboard(token);
    }
}

fn peer_url(input: &str) -> Result<reqwest::Url, ()> {
    let mut url = reqwest::Url::parse(input.trim()).map_err(|_| ())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(());
    }
    url.set_path("/");
    Ok(url)
}

async fn get<T: serde::de::DeserializeOwned>(
    url: reqwest::Url,
    token: &str,
) -> Result<T, &'static str> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "verify-failed")?;
    let mut response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "verify-failed")?;
    if !response.status().is_success() {
        return Err(if response.status().as_u16() == 409 {
            "host-changed"
        } else {
            "verify-failed"
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "verify-failed")? {
        if bytes.len() + chunk.len() > 1_048_576 {
            return Err("verify-failed");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "verify-failed")
}

pub fn verify_host(url: String, token: String) {
    if !enabled() {
        return;
    }
    let Ok(url) = peer_url(&url) else {
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            state.peer_generation += 1;
            state.revision += 1;
            state.peer = None;
            state.page = None;
            state.probing = false;
            state.searching = false;
            state.status = "invalid-address";
        }
        publish();
        return;
    };
    let generation = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.peer_generation += 1;
        state.revision += 1;
        state.peer = None;
        state.page = None;
        state.probing = true;
        state.searching = false;
        state.status = "";
        state.peer_generation
    };
    publish();
    crate::spawn(async move {
        let result = get::<LibraryInfo>(
            url.join("api/orbit/library")
                .expect("constant relative URL"),
            &token,
        )
        .await;
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            if state.peer_generation != generation {
                return;
            }
            state.probing = false;
            state.revision += 1;
            match result {
                Ok(info)
                    if info.protocol == PROTOCOL_VERSION
                        && !info.instance.is_empty()
                        && info
                            .capabilities
                            .iter()
                            .any(|c| c == "library.files.inspect") =>
                {
                    state.peer = Some(Peer { url, token, info });
                    state.status = "verified";
                }
                Ok(_) => state.status = "not-supported",
                Err(error) => state.status = error,
            }
        }
        publish();
    });
}

pub fn search_library(query: String) {
    if !enabled() {
        return;
    }
    let (peer, generation) = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(peer) = state
            .peer
            .as_ref()
            .filter(|p| {
                p.info
                    .capabilities
                    .iter()
                    .any(|c| c == "library.files.search")
            })
            .cloned()
        else {
            return;
        };
        state.peer_generation += 1;
        state.revision += 1;
        state.searching = true;
        state.page = None;
        state.status = "";
        (peer, state.peer_generation)
    };
    publish();
    crate::spawn(async move {
        let mut url = peer
            .url
            .join("api/orbit/library/search")
            .expect("constant relative URL");
        url.query_pairs_mut()
            .append_pair("instance", &peer.info.instance)
            .append_pair("q", &query)
            .append_pair("limit", "25");
        let result = get::<LibraryPage>(url, &peer.token).await;
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            if state.peer_generation != generation {
                return;
            }
            state.searching = false;
            state.revision += 1;
            match result {
                Ok(page) if page.instance == peer.info.instance => state.page = Some(page),
                Ok(_) => {
                    state.peer = None;
                    state.status = "host-changed";
                }
                Err(error) => {
                    state.peer = None;
                    state.status = error;
                }
            }
        }
        publish();
    });
}
fn set_status(status: &'static str) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.status = status;
    state.revision += 1;
    drop(state);
    publish();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn orbit_requires_the_exact_launch_flag() {
        assert!(!has_flag(
            ["qbz", "--no-orbit", "--orbit=false"].map(String::from)
        ));
        assert!(has_flag(["qbz", "--orbit"].map(String::from)));
    }
    #[test]
    fn peer_address_cannot_smuggle_credentials_or_a_different_route() {
        for bad in [
            "file:///tmp/qbz",
            "http://user:pass@host/",
            "http://host/api",
            "http://host/?token=secret",
            "http://host/#key",
        ] {
            assert!(peer_url(bad).is_err(), "{bad}");
        }
        for good in [
            "http://127.0.0.1:17290",
            "http://[::1]:17290/",
            "https://music.example/",
        ] {
            assert!(peer_url(good).is_ok(), "{good}");
        }
    }
}
