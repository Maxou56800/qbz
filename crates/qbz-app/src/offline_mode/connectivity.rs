//! Connectivity actor — the robust replacement for Tauri's poll-only checker.
//!
//! Tauri's checker (issue #467 era) raced three hostname-based generate_204
//! probes from a frontend `setInterval`, with a process-global 2-consecutive-
//! failure counter. Its residual false-offline vectors (review report 02 §4):
//! every probe needs DNS, two of three endpoints are Google infra (Pi-hole),
//! unspaced counting defeats the hysteresis, nothing resets on suspend/resume,
//! and captive-portal redirects count as ONLINE.
//!
//! This actor is Rust-owned and layered (spec §3.2):
//!
//! 1. **OS route signal (Linux)** — no IPv4/IPv6 default route ⇒ `Down`
//!    immediately, no probe needed (pulling the cable / wifi off detects in
//!    one tick instead of two failed 8s probes). Sandbox-safe `/proc` reads.
//! 2. **Passive liveness** — audio bytes flowing within the last 45 s ⇒ `Up`
//!    by definition (`qbz_audio::network_throttle`), no probe traffic while
//!    streaming. Same rule as #467's Fix E.
//! 3. **Hardened probe set** — one IP-LITERAL probe (DNS-independent — the
//!    DNS-hiccup false-offline vector dies here), vendor diversity (Cloudflare
//!    / Google / Microsoft), strict response validation, and redirects count
//!    as CAPTIVE PORTAL, never as success.
//! 4. **Asymmetric hysteresis** — one confirmed success flips `Up` instantly;
//!    flipping `Down` from `Up` requires a fresh CONFIRMATION BURST (immediate
//!    short re-probes) so a single lost race never declares offline, and the
//!    confirmation is time-bounded (stale failures don't count).
//! 5. **Suspend/resume guard** — a wall-clock jump without matching monotonic
//!    progress discards accumulated failures before judging.
//!
//! State broadcasts over a `tokio::sync::watch` channel; the offline-mode
//! engine subscribes and derives the app mode from it.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::watch;

/// Raw connectivity verdict, independent of the app's offline MODE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connectivity {
    Up,
    Down,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectivitySnapshot {
    pub state: Connectivity,
    /// A probe was answered by a redirect — typical captive portal. Surfaced
    /// as a hint; the state is still `Down` (D3: a portal cannot reach Qobuz).
    pub captive_portal: bool,
}

impl Default for ConnectivitySnapshot {
    fn default() -> Self {
        Self {
            state: Connectivity::Unknown,
            captive_portal: false,
        }
    }
}

/// How a single probe endpoint decides success. Strict on purpose: Tauri
/// accepted any 2xx/3xx, which read captive portals as online.
enum ProbeExpect {
    /// Exactly HTTP 204, empty-ish body (generate_204 contract).
    Status204,
    /// HTTP 200 and the body contains the marker.
    BodyContains(&'static str),
}

struct ProbeEndpoint {
    url: &'static str,
    expect: ProbeExpect,
}

/// Vendor-diverse probe set. The first entry is IP-literal: it works with
/// DNS completely broken, which was the most likely residual false-offline
/// vector after #467 (all three old endpoints were hostname-based).
const PROBES: &[ProbeEndpoint] = &[
    ProbeEndpoint {
        url: "https://1.1.1.1/cdn-cgi/trace",
        expect: ProbeExpect::BodyContains("ip="),
    },
    ProbeEndpoint {
        url: "https://connectivitycheck.gstatic.com/generate_204",
        expect: ProbeExpect::Status204,
    },
    ProbeEndpoint {
        // Microsoft NCSI defines this probe as plain HTTP. HTTPS is not a
        // supported equivalent: the CDN can answer with an Akamai certificate
        // that is not valid for www.msftconnecttest.com, producing a rustls
        // error before the strict body check can run.
        url: "http://www.msftconnecttest.com/connecttest.txt",
        expect: ProbeExpect::BodyContains("Microsoft Connect Test"),
    },
];

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// Seconds of audio-segment silence within which we are online by definition.
const LIVENESS_WINDOW_SECS: u64 = 45;
/// Regular evaluation cadence while nothing changes.
const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Confirmation burst delays after a failed probe while `Up` (verify before
/// flipping down — replaces Tauri's "2 polls ~60 s" with ~10 s of focused
/// re-checking that can't be gamed by unspaced extra polls).
const CONFIRM_DELAYS: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(7)];
/// A wall-clock jump this much larger than monotonic progress = we slept.
const RESUME_JUMP: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    Success,
    /// All endpoints failed (timeout / error / wrong payload).
    Failure,
    /// At least one endpoint answered with a redirect and none succeeded.
    CaptivePortal,
}

/// Pure decision core — injected outcomes, no sockets. Unit-testable.
#[derive(Debug)]
pub struct ConnectivityJudge {
    snapshot: ConnectivitySnapshot,
    /// Pending down-confirmation: how many burst steps are left.
    confirm_steps_left: usize,
    /// When the failing streak started (time-bounds the confirmation).
    first_failure_at: Option<Instant>,
}

/// What the actor should do after feeding the judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeAction {
    /// Nothing pending — sleep until the next regular tick.
    Idle,
    /// Re-probe after the given delay (confirmation burst step).
    ConfirmAfter(Duration),
}

impl ConnectivityJudge {
    pub fn new() -> Self {
        Self {
            snapshot: ConnectivitySnapshot::default(),
            confirm_steps_left: 0,
            first_failure_at: None,
        }
    }

    pub fn snapshot(&self) -> ConnectivitySnapshot {
        self.snapshot
    }

    /// A definitive OS-level signal: no default route at all.
    pub fn on_no_route(&mut self) {
        self.confirm_steps_left = 0;
        self.first_failure_at = None;
        self.snapshot = ConnectivitySnapshot {
            state: Connectivity::Down,
            captive_portal: false,
        };
    }

    /// Audio bytes (or other Qobuz traffic) observed recently.
    pub fn on_liveness(&mut self) {
        self.confirm_steps_left = 0;
        self.first_failure_at = None;
        self.snapshot = ConnectivitySnapshot {
            state: Connectivity::Up,
            captive_portal: false,
        };
    }

    /// Discard any failing streak (suspend/resume, manual mode change).
    pub fn reset_streak(&mut self) {
        self.confirm_steps_left = 0;
        self.first_failure_at = None;
    }

    pub fn on_probe(&mut self, outcome: ProbeOutcome, now: Instant) -> JudgeAction {
        match outcome {
            ProbeOutcome::Success => {
                // Asymmetric: one confirmed success flips Up instantly.
                self.confirm_steps_left = 0;
                self.first_failure_at = None;
                self.snapshot = ConnectivitySnapshot {
                    state: Connectivity::Up,
                    captive_portal: false,
                };
                JudgeAction::Idle
            }
            ProbeOutcome::Failure | ProbeOutcome::CaptivePortal => {
                let captive = outcome == ProbeOutcome::CaptivePortal;
                match self.snapshot.state {
                    Connectivity::Up => {
                        // Was up: never flip on one loss — start/advance the
                        // confirmation burst.
                        match self.first_failure_at {
                            None => {
                                self.first_failure_at = Some(now);
                                self.confirm_steps_left = CONFIRM_DELAYS.len();
                            }
                            Some(start) => {
                                // Time-bound: a stale streak (e.g. ticks that
                                // straddled a suspend) restarts instead of
                                // accumulating.
                                if now.duration_since(start) > Duration::from_secs(120) {
                                    self.first_failure_at = Some(now);
                                    self.confirm_steps_left = CONFIRM_DELAYS.len();
                                }
                            }
                        }
                        if self.confirm_steps_left > 0 {
                            let idx = CONFIRM_DELAYS.len() - self.confirm_steps_left;
                            self.confirm_steps_left -= 1;
                            JudgeAction::ConfirmAfter(CONFIRM_DELAYS[idx])
                        } else {
                            // Burst exhausted and still failing: confirmed down.
                            self.snapshot = ConnectivitySnapshot {
                                state: Connectivity::Down,
                                captive_portal: captive,
                            };
                            self.first_failure_at = None;
                            JudgeAction::Idle
                        }
                    }
                    Connectivity::Down | Connectivity::Unknown => {
                        // Already down (or first-ever verdict): no burst needed.
                        self.snapshot = ConnectivitySnapshot {
                            state: Connectivity::Down,
                            captive_portal: captive,
                        };
                        JudgeAction::Idle
                    }
                }
            }
        }
    }
}

impl Default for ConnectivityJudge {
    fn default() -> Self {
        Self::new()
    }
}

// ===================== OS route signal (Linux) =====================

/// Parse `/proc/net/route` content: any non-loopback entry with destination
/// 00000000 is an IPv4 default route.
fn ipv4_has_default_route(content: &str) -> bool {
    content.lines().skip(1).any(|line| {
        let mut cols = line.split_whitespace();
        let iface = cols.next().unwrap_or("");
        let dest = cols.next().unwrap_or("");
        iface != "lo" && dest == "00000000"
    })
}

/// Parse `/proc/net/ipv6_route` content: any non-loopback entry with
/// destination ::/0 (32 zero hex chars, prefix length 00) is a default route.
fn ipv6_has_default_route(content: &str) -> bool {
    content.lines().any(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 {
            return false;
        }
        let dest = cols[0];
        let prefix_len = cols[1];
        let iface = cols[9];
        iface != "lo"
            && prefix_len == "00"
            && dest.len() == 32
            && dest.bytes().all(|b| b == b'0')
    })
}

/// `Some(true)` = at least one default route exists; `Some(false)` = readable
/// and definitely none; `None` = signal unavailable (non-Linux or read error)
/// — the caller falls back to probes only.
pub fn has_default_route() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let v4 = std::fs::read_to_string("/proc/net/route")
            .map(|c| ipv4_has_default_route(&c))
            .ok();
        let v6 = std::fs::read_to_string("/proc/net/ipv6_route")
            .map(|c| ipv6_has_default_route(&c))
            .ok();
        match (v4, v6) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(false) || b.unwrap_or(false)),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

// ===================== Probing =====================

async fn probe_endpoint(client: &reqwest::Client, ep: &ProbeEndpoint) -> ProbeOutcome {
    match client.get(ep.url).send().await {
        Ok(response) => {
            let status = response.status();
            if status.is_redirection() {
                return ProbeOutcome::CaptivePortal;
            }
            match ep.expect {
                ProbeExpect::Status204 => {
                    if status.as_u16() == 204 {
                        ProbeOutcome::Success
                    } else {
                        ProbeOutcome::Failure
                    }
                }
                ProbeExpect::BodyContains(marker) => {
                    if status.as_u16() != 200 {
                        return ProbeOutcome::Failure;
                    }
                    match response.text().await {
                        Ok(body) if body.contains(marker) => ProbeOutcome::Success,
                        _ => ProbeOutcome::Failure,
                    }
                }
            }
        }
        Err(_) => ProbeOutcome::Failure,
    }
}

/// Race the probe set; first validated success wins. Redirect answers are
/// remembered: all-fail + any-redirect = captive portal.
pub async fn probe_all(client: &reqwest::Client) -> ProbeOutcome {
    let mut set = tokio::task::JoinSet::new();
    for ep in PROBES {
        let client = client.clone();
        set.spawn(async move { probe_endpoint(&client, ep).await });
    }

    let mut saw_captive = false;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(ProbeOutcome::Success) => {
                set.abort_all();
                return ProbeOutcome::Success;
            }
            Ok(ProbeOutcome::CaptivePortal) => saw_captive = true,
            _ => {}
        }
    }
    if saw_captive {
        ProbeOutcome::CaptivePortal
    } else {
        ProbeOutcome::Failure
    }
}

fn audio_liveness_recent() -> bool {
    qbz_audio::network_throttle::state()
        .seconds_since_download()
        .map(|secs| secs <= LIVENESS_WINDOW_SECS)
        .unwrap_or(false)
}

// ===================== Actor =====================

/// Handle to the running actor: subscribe to state, poke a recheck.
pub struct ConnectivityActor {
    rx: watch::Receiver<ConnectivitySnapshot>,
    recheck: tokio::sync::mpsc::Sender<()>,
}

impl ConnectivityActor {
    /// Spawn the actor loop on the current tokio runtime.
    pub fn spawn() -> Self {
        let (tx, rx) = watch::channel(ConnectivitySnapshot::default());
        let (recheck_tx, mut recheck_rx) = tokio::sync::mpsc::channel::<()>(4);

        tokio::spawn(async move {
            // Routed through the same proxy as every other request: probing
            // direct connectivity while the rest of the app goes through a
            // proxy would report "online"/"offline" against the wrong path.
            let client =
                match qbz_net_proxy::apply_current(reqwest::Client::builder()).and_then(|builder| {
                    builder
                        .timeout(PROBE_TIMEOUT)
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .map_err(Into::into)
                }) {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("[Connectivity] probe client build failed: {}", e);
                        return;
                    }
                };

            let mut judge = ConnectivityJudge::new();
            let mut next_delay = Duration::from_millis(10); // first verdict ASAP
            let mut last_tick_wall = SystemTime::now();
            let mut last_tick_mono = Instant::now();

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(next_delay) => {}
                    poke = recheck_rx.recv() => {
                        if poke.is_none() { return; }
                        judge.reset_streak();
                    }
                }

                // Suspend/resume guard: wall-clock advanced much further than
                // the monotonic clock ⇒ we slept; discard the streak.
                let wall_delta = SystemTime::now()
                    .duration_since(last_tick_wall)
                    .unwrap_or_default();
                let mono_delta = last_tick_mono.elapsed();
                if wall_delta > mono_delta + RESUME_JUMP {
                    log::info!("[Connectivity] resume detected, resetting failure streak");
                    judge.reset_streak();
                }
                last_tick_wall = SystemTime::now();
                last_tick_mono = Instant::now();

                // Layer 1: OS route signal — definitive Down.
                if has_default_route() == Some(false) {
                    judge.on_no_route();
                    let _ = tx.send_if_modified(|s| {
                        let changed = *s != judge.snapshot();
                        *s = judge.snapshot();
                        changed
                    });
                    next_delay = Duration::from_secs(3); // cheap; watch for the route to return
                    continue;
                }

                // Layer 2: passive liveness — definitive Up, zero traffic.
                if audio_liveness_recent() {
                    judge.on_liveness();
                    let _ = tx.send_if_modified(|s| {
                        let changed = *s != judge.snapshot();
                        *s = judge.snapshot();
                        changed
                    });
                    next_delay = POLL_INTERVAL;
                    continue;
                }

                // Layer 3: probe + hysteresis.
                let outcome = probe_all(&client).await;
                let action = judge.on_probe(outcome, Instant::now());
                let _ = tx.send_if_modified(|s| {
                    let changed = *s != judge.snapshot();
                    *s = judge.snapshot();
                    changed
                });
                next_delay = match action {
                    JudgeAction::ConfirmAfter(delay) => delay,
                    JudgeAction::Idle => POLL_INTERVAL,
                };
            }
        });

        Self {
            rx,
            recheck: recheck_tx,
        }
    }

    /// Subscribe to connectivity state changes.
    pub fn subscribe(&self) -> watch::Receiver<ConnectivitySnapshot> {
        self.rx.clone()
    }

    /// Current snapshot without subscribing.
    pub fn snapshot(&self) -> ConnectivitySnapshot {
        *self.rx.borrow()
    }

    /// Force an immediate re-evaluation (Settings "Check now", resume hooks,
    /// mode changes). Also clears any failing streak first.
    pub fn request_recheck(&self) {
        let _ = self.recheck.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn first_success_goes_up_immediately() {
        let mut judge = ConnectivityJudge::new();
        assert_eq!(judge.on_probe(ProbeOutcome::Success, now()), JudgeAction::Idle);
        assert_eq!(judge.snapshot().state, Connectivity::Up);
    }

    #[test]
    fn single_failure_while_up_does_not_flip_down() {
        let mut judge = ConnectivityJudge::new();
        judge.on_probe(ProbeOutcome::Success, now());

        let action = judge.on_probe(ProbeOutcome::Failure, now());
        assert!(matches!(action, JudgeAction::ConfirmAfter(_)));
        assert_eq!(
            judge.snapshot().state,
            Connectivity::Up,
            "must stay Up during the confirmation burst"
        );
    }

    #[test]
    fn exhausted_confirmation_burst_flips_down() {
        let mut judge = ConnectivityJudge::new();
        judge.on_probe(ProbeOutcome::Success, now());

        // burst steps: CONFIRM_DELAYS.len() ConfirmAfter actions, then Down.
        let mut flips = 0;
        for _ in 0..(CONFIRM_DELAYS.len() + 1) {
            if judge.on_probe(ProbeOutcome::Failure, now()) == JudgeAction::Idle {
                flips += 1;
            }
        }
        assert_eq!(flips, 1);
        assert_eq!(judge.snapshot().state, Connectivity::Down);
    }

    #[test]
    fn success_mid_burst_cancels_the_flip() {
        let mut judge = ConnectivityJudge::new();
        judge.on_probe(ProbeOutcome::Success, now());
        judge.on_probe(ProbeOutcome::Failure, now());
        judge.on_probe(ProbeOutcome::Success, now());
        assert_eq!(judge.snapshot().state, Connectivity::Up);

        // The next failure starts a FRESH burst (streak was reset).
        let action = judge.on_probe(ProbeOutcome::Failure, now());
        assert!(matches!(action, JudgeAction::ConfirmAfter(_)));
        assert_eq!(judge.snapshot().state, Connectivity::Up);
    }

    #[test]
    fn down_recovers_on_single_success() {
        let mut judge = ConnectivityJudge::new();
        judge.on_probe(ProbeOutcome::Failure, now());
        assert_eq!(judge.snapshot().state, Connectivity::Down);

        judge.on_probe(ProbeOutcome::Success, now());
        assert_eq!(judge.snapshot().state, Connectivity::Up);
    }

    #[test]
    fn captive_portal_is_down_with_hint() {
        let mut judge = ConnectivityJudge::new();
        judge.on_probe(ProbeOutcome::CaptivePortal, now());
        assert_eq!(judge.snapshot().state, Connectivity::Down);
        assert!(judge.snapshot().captive_portal);
    }

    #[test]
    fn no_route_is_immediate_down() {
        let mut judge = ConnectivityJudge::new();
        judge.on_probe(ProbeOutcome::Success, now());
        judge.on_no_route();
        assert_eq!(judge.snapshot().state, Connectivity::Down);
    }

    #[test]
    fn liveness_is_immediate_up_and_clears_streak() {
        let mut judge = ConnectivityJudge::new();
        judge.on_probe(ProbeOutcome::Success, now());
        judge.on_probe(ProbeOutcome::Failure, now()); // burst started
        judge.on_liveness();
        assert_eq!(judge.snapshot().state, Connectivity::Up);

        // Fresh burst required again.
        let action = judge.on_probe(ProbeOutcome::Failure, now());
        assert!(matches!(action, JudgeAction::ConfirmAfter(_)));
    }

    #[test]
    fn ipv4_route_parse() {
        let with_default = "Iface\tDestination\tGateway\tFlags\n\
                            wlan0\t00000000\t0102A8C0\t0003\n\
                            wlan0\t0002A8C0\t00000000\t0001\n";
        let without_default = "Iface\tDestination\tGateway\tFlags\n\
                               wlan0\t0002A8C0\t00000000\t0001\n";
        let lo_only = "Iface\tDestination\tGateway\tFlags\n\
                       lo\t00000000\t00000000\t0001\n";
        assert!(ipv4_has_default_route(with_default));
        assert!(!ipv4_has_default_route(without_default));
        assert!(!ipv4_has_default_route(lo_only));
    }

    #[test]
    fn ipv6_route_parse() {
        // dest(32) prefix(2) src(32) srcprefix(2) nexthop(32) metric refcnt use flags iface
        let with_default = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000000 00000000 00000003 wlan0\n";
        let non_default = "20010db8000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000400 00000000 00000000 00000001 wlan0\n";
        let lo_default = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 00000400 00000000 00000000 00000003 lo\n";
        assert!(ipv6_has_default_route(with_default));
        assert!(!ipv6_has_default_route(non_default));
        assert!(!ipv6_has_default_route(lo_default));
    }

    #[test]
    fn microsoft_ncsi_probe_uses_its_defined_http_transport() {
        let endpoint = PROBES
            .iter()
            .find(|endpoint| {
                matches!(
                    endpoint.expect,
                    ProbeExpect::BodyContains("Microsoft Connect Test")
                )
            })
            .expect("Microsoft NCSI probe");
        assert_eq!(
            endpoint.url,
            "http://www.msftconnecttest.com/connecttest.txt"
        );
    }
}
