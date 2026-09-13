// crates/qbzd/src/tui/strings.rs — EVERY user-facing string in the setup TUI.
//
// English-only v1 (03-setup-tui.md §1.2). Centralized here so a later gettext
// pass (qbz-i18n is Slint-free) is a P2 batch job, not a rewrite. Nothing under
// tui/ prints a bare literal — it comes from here.

// ============================ entry / guards ============================

/// Non-tty rejection (03 §2.4, VERBATIM). Printed to stderr, exit 2.
pub const NON_TTY_ERROR: &str = "error: 'qbzd setup' needs an interactive terminal
  → import a settings bundle:   qbzd settings import <file.qbzb>
  → set one value:              qbzd settings set <key> <value>
  → log in without the TUI:     qbzd login   (or: qbzd login --token <token>)";

/// Terminal-too-small line (03 §5.4). `w`/`h` are the current dimensions.
pub fn too_small(w: u16, h: u16) -> String {
    format!("terminal too small — 80×24 minimum (current: {w}×{h})")
}

// ============================ shell / navigation ============================

/// Header title (accent-bold, left of the version). One row, always visible.
pub const APP_TITLE: &str = "QBZ Daemon Setup";
pub const HELP_TITLE: &str = "Help";

/// Breadcrumb root node (dim `Setup ›` prefix). The current node (section or,
/// while editing, the field label) carries the accent.
pub const BREADCRUMB_ROOT: &str = "Setup";

/// Persistent left-nav labels (fixed order), COMPACT tier (< 100 cols). NAME
/// only; Dirty-capable sections (Audio/Playback/QConnect/Network) stay ≤ 8 chars
/// so a trailing `*` fits the 14-col sidebar; Account/Import/Wizard are never
/// dirty. Seven since FB4 added the HiFi Wizard (owner-sanctioned cap break over
/// the old six-screen D7 cap).
pub const SIDEBAR_LABELS: [&str; 8] = [
    "Account",
    "Audio",
    "Playback",
    "QConnect",
    "Network",
    "Import/Exp",
    "Wizard",
    "Scrobbler",
];

/// Left-nav labels, WIDE tier (≥ 100 cols → the 28-col sidebar, FB5). The extra
/// room lets `Import / Export` spell itself out; everything else is already whole.
pub const SIDEBAR_LABELS_WIDE: [&str; 8] = [
    "Account",
    "Audio",
    "Playback",
    "QConnect",
    "Network",
    "Import / Export",
    "Wizard",
    "Scrobbler",
];

/// A terse, static one-line summary shown dim UNDER each name in the wide sidebar
/// only (FB5). Static (not live device state) — it names what the section is for
/// so the roomy sidebar reads intentionally; it never needs the section loaded.
pub const SIDEBAR_SUMMARIES: [&str; 8] = [
    "sign in",
    "output · bit-perfect",
    "quality · behavior",
    "cast target",
    "http control",
    "settings bundle",
    "DAC setup",
    "last.fm · listenbrainz",
];

// Global help-bar hints (context-sensitive; assembled per focus + screen).
pub const HELP_NAV: &str = "up/down move · Enter open · 1-8 jump · Tab content · ? help · q quit";
pub const HELP_CONTENT_CLEAN: &str = "up/down move · Enter edit · Tab nav · Esc nav · ? help · q quit";
pub const HELP_CONTENT_DIRTY: &str = "up/down move · Enter edit · s SAVE* · Tab nav · Esc nav · q quit";
pub const HELP_AUDIO_CLEAN: &str =
    "up/down move · Enter edit · r refresh · / filter · Tab nav · Esc nav";
pub const HELP_AUDIO_DIRTY: &str =
    "up/down move · s SAVE* · r refresh · / filter · Tab nav · Esc nav";
pub const HELP_SELECT: &str = "up/down choose · Enter select · Esc cancel";
pub const HELP_FILTER: &str = "type to filter · up/down choose · Enter select · Esc cancel";
pub const HELP_INPUT: &str = "type · Enter accept · Esc cancel";

pub const HELP_OVERLAY: &str = "GLOBAL KEYS

  up / down (or j / k)   move (sidebar or field)
  Enter                  open a section / edit a field / confirm
  Tab                    toggle sidebar <-> content
  Esc                    content: back to sidebar · sidebar: quit
  1 - 7                  jump straight to a section
  s                      save the current section
  r                      refresh (Audio: re-enumerate devices)
  /                      filter (device picker)
  ?                      this help
  q                      quit (asks to save unsaved changes)

Each section saves explicitly with 's'. A '*' by a section name means unsaved
edits. Leaving a dirty section asks to save first. The daemon does NOT need to
be running — changes apply when it next starts.

  Press Esc or ? to close.";

// ============================ dirty-save / quit ============================

pub const DIRTY_TITLE: &str = "Unsaved changes";
pub const DIRTY_BODY: &str = "This screen has unsaved edits.";
pub const DIRTY_HINT: &str = "s save · d discard · Esc stay";

// ============================ footer (daemon state) ============================

pub const FOOTER_UNREACHABLE: &str = "daemon: not reachable";
pub const FOOTER_RUNNING: &str = "daemon: running";
pub const FOOTER_NEEDS_AUTH: &str = "not signed in";
/// Appended to a save result when the daemon is down (03 §2.3, error-voice).
pub const APPLIES_ON_START: &str =
    "changes apply when the daemon starts — systemctl --user status qbzd";

// ============================ Account (§3.1) ============================

pub const ACCOUNT_TITLE: &str = "Account";
/// In-screen section box title (distinct from the screen title in the frame).
pub const ACCOUNT_SECTION: &str = "SIGN-IN";
pub const ACCOUNT_STATUS: &str = "Status";
pub const ACCOUNT_NOT_LOGGED_IN: &str = "not logged in";
/// Offline + daemon-down: a credential file exists but was never validated —
/// NEVER fabricate an email/name (§3.1 rules).
pub const ACCOUNT_CRED_PRESENT: &str = "credential file present (not validated)";
pub const ACCOUNT_LOGIN_BROWSER: &str = "Log in with browser";
pub const ACCOUNT_PASTE_TOKEN: &str = "Paste token";
pub const ACCOUNT_LOGOUT: &str = "Log out";

pub fn account_logged_in(email: &str) -> String {
    format!("logged in as {email}")
}
pub fn account_logged_in_plan(email: &str, plan: &str) -> String {
    format!("logged in as {email} ({plan})")
}

pub const ACCOUNT_LOGOUT_CONFIRM_TITLE: &str = "Log out";
pub const ACCOUNT_LOGOUT_CONFIRM_BODY: &str =
    "Clear the Qobuz credentials on this box? If the daemon is running it will\nstop playback and wait for a new login.";
pub const CONFIRM_YN: &str = "y confirm · Esc cancel";

pub const ACCOUNT_VALIDATING: &str = "validating token with Qobuz…";

/// Suspend-and-run divergence banner (see report): the browser flow runs on the
/// plain terminal. Shown briefly before the alt-screen is left.
pub const ACCOUNT_BROWSER_HANDOFF: &str =
    "Starting browser login on the terminal below. Follow the printed URL;\nthe TUI resumes when login finishes or times out.";

// ============================ Audio (§3.2) ============================

pub const AUDIO_TITLE: &str = "Audio";
pub const AUDIO_GROUP_OUTPUT: &str = "OUTPUT";
pub const AUDIO_GROUP_BITPERFECT: &str = "BIT-PERFECT";
pub const AUDIO_GROUP_TRANSPORT: &str = "STREAMING TRANSPORT";

pub const A_BACKEND: &str = "Backend";
pub const A_DEVICE: &str = "Output device";
pub const A_ALSA_PLUGIN: &str = "ALSA plugin";
pub const A_HW_VOLUME: &str = "Hardware volume";
pub const A_HW_VOLUME_CONTROL: &str = "Mixer control";
pub const A_DSD: &str = "DSD playback";
pub const A_EXCLUSIVE: &str = "Exclusive mode";
pub const A_RESERVE: &str = "Reserve DAC";
pub const A_PASSTHROUGH: &str = "DAC passthrough";
pub const A_FORCE_BP: &str = "Force bit-perfect";
pub const A_LOCK_OUTPUT: &str = "Lock output device";
pub const A_STREAM_UNCACHED: &str = "Stream uncached";
pub const A_BUFFER: &str = "Initial buffer";
pub const A_STREAMING_ONLY: &str = "Streaming only";

// Disabled-row reasons (rendered dim in parentheses, §3).
pub const R_ALSA_ONLY: &str = "ALSA only";
pub const R_PIPEWIRE_ONLY: &str = "PipeWire only";
pub const R_PASSTHROUGH_OFF: &str = "off while DAC passthrough on";

pub const DSD_CONVERT: &str = "Convert to PCM (works everywhere)";
pub const DSD_DOP: &str = "DoP — DSD over PCM (bit-perfect)";
pub const DSD_NATIVE: &str = "Native DSD (kernel support required)";

pub const ALSA_HW: &str = "hw (Direct Hardware)";
pub const ALSA_PLUGHW: &str = "plughw (Auto-convert)";
pub const ALSA_PCM: &str = "pcm (Most compatible)";

/// DSD guard (§3.2.4). Verbatim-in-spirit of the desktop warning.
pub const DSD_GUARD_TITLE: &str = "DSD direct mode";
pub const DSD_GUARD_BODY: &str = "Choose DoP or Native only if your DAC supports it. On any other DAC they play\nas LOUD NOISE. Volume is fixed in DoP/Native; Native additionally needs\nkernel support.";
pub const DSD_GUARD_HINT: &str = "Enter confirm · Esc revert";

pub const AUDIO_SCANNING: &str = "scanning…";
pub const DEVICE_PICKER_TITLE: &str = "Output device";

/// No-devices hint panel (§5.1).
pub const NO_DEVICES: &str = "no output devices found — is the DAC plugged in and powered? is your user in\nthe 'audio' group? PipeWire backend: is pipewire running?  (r to re-scan)";

pub const JACK_WARNING: &str = "JACK is not bit-perfect (routes through the JACK graph, resamples)";
pub const BP_BADGE: &str = "[BP]";

// ============================ Playback (§3.3) ============================

pub const PLAYBACK_TITLE: &str = "Playback";
pub const PLAYBACK_GROUP_QUALITY: &str = "QUALITY";
pub const PLAYBACK_GROUP_BEHAVIOR: &str = "BEHAVIOR";
pub const PLAYBACK_GROUP_SESSION: &str = "SESSION";
pub const PLAYBACK_GROUP_CONTROLS: &str = "MEDIA CONTROLS";

pub const P_QUALITY: &str = "Streaming quality";
pub const P_LIMIT_DEVICE: &str = "Limit quality to device";
pub const P_MAX_RATE: &str = "Maximum sample rate";
pub const P_ALLOW_FALLBACK: &str = "Allow quality fallback";
pub const P_RETRY_FAIL: &str = "When retries fail";
pub const P_CONTINUE: &str = "Continue after track";
pub const P_GAPLESS: &str = "Gapless playback";
pub const P_RESTORE: &str = "Restore session";
pub const P_RESUME_POS: &str = "Resume position";
pub const P_MPRIS: &str = "System media controls";
pub const P_MPRIS_DESC: &str = "MPRIS: KDE/GNOME media widget + media keys · restart to apply";

pub const R_LIMIT_OFF: &str = "Limit quality to device off";
pub const R_RESTORE_OFF: &str = "needs Restore session";

pub const Q_MP3: &str = "MP3";
pub const Q_CD: &str = "CD Quality";
pub const Q_HIRES: &str = "Hi-Res";
pub const Q_HIRES_PLUS: &str = "Hi-Res+";

pub const RETRY_FALLBACK: &str = "Fall back (play lowest available)";
pub const RETRY_SKIP: &str = "Skip the track";
/// Stored `ask` render until the operator picks (§3.3.2). The TUI never writes `ask`.
pub const RETRY_ASK: &str = "Ask (desktop setting) — daemon falls back";

pub const AUTOPLAY_ON: &str = "on";
pub const AUTOPLAY_OFF: &str = "off";
/// A pre-existing `infinite` (radio, P1) renders read-only until toggled (§3.3.1).
pub const AUTOPLAY_INFINITE: &str = "on (infinite radio)";

pub const RATE_NO_LIMIT: &str = "No limit";

// ============================ QConnect (§3.4) ============================

pub const QCONNECT_TITLE: &str = "Qobuz Connect";
/// In-screen section box title.
pub const QCONNECT_SECTION: &str = "CONNECTION";
pub const QC_ENABLE: &str = "Enable";
pub const QC_DEVICE_NAME: &str = "Device name";
pub const QC_VOLUME_MODE: &str = "Volume mode";
pub const QC_APPLIES_NEXT: &str = "applies on the next connection";
pub const VOL_SOFTWARE: &str = "software";
pub const VOL_LOCKED: &str = "locked";

pub fn qc_preview(name: &str) -> String {
    format!("phones will see: \"{name}\"")
}

// ============================ Network (§3.5) ============================

pub const NETWORK_TITLE: &str = "Network";
/// In-screen section box title.
pub const NETWORK_SECTION: &str = "HTTP SERVER";
pub const N_BIND: &str = "Bind address";
pub const N_PORT: &str = "Port";
pub const N_TOKEN: &str = "Access token";
pub const N_TOKEN_HINT: &str = "(empty = open)";

/// LAN-first posture note shown when bind is non-loopback (§3.5, copy normative).
pub const NETWORK_LAN_POSTURE: &str = "open LAN control (Sonos/Chromecast posture) — anyone on your network can control playback\n  restrict: bind = \"127.0.0.1\" or set [server] token in qbzd.toml";

/// Restart-required copy on a bind/port/token save (§3.5).
pub const NETWORK_RESTART: &str =
    "bind/port change needs a restart — systemctl --user restart qbzd";

pub const N_BAD_IP: &str = "invalid IP address";
pub const N_BAD_PORT: &str = "port must be 1-65535";

/// Pre-save warning naming keys outside the daemon schema. Per 03 §3.5 they are
/// PRESERVED on save (a save must never destroy a released key) — only comments
/// and formatting are lost. (The brief said "drops"; 03 wins — flagged in the
/// report.) Keys are appended.
pub const N_DROP_UNKNOWN: &str =
    "note: qbzd.toml has keys outside the daemon schema — they are kept, but\n  comments and formatting are not preserved on save:";

// ============================ Import / Export (§3.6) ============================

pub const BUNDLE_TITLE: &str = "Import / Export";
pub const BUNDLE_IMPORT_HEADER: &str = "IMPORT";
pub const BUNDLE_EXPORT_HEADER: &str = "EXPORT";

pub const B_IMPORT_PATH: &str = "Bundle file";
pub const B_IMPORT_PATH_HINT: &str = "path to a .qbzb (scp it to ~ first)";
pub const B_IMPORT_ACTION: &str = "Review import";
pub const B_EXPORT_DEST: &str = "Destination";
pub const B_EXPORT_INCLUDE_AUTH: &str = "Include Qobuz login";
pub const B_EXPORT_ACTION: &str = "Export";

pub const B_BUCKET_APPLIED: &str = "applies verbatim";
pub const B_BUCKET_ADAPTED: &str = "needs your confirmation";
pub const B_BUCKET_SKIPPED: &str = "skipped";

/// Import-side auth gate (§3.6 step 5) — dedicated, default-OFF.
pub const B_IMPORT_AUTH_TITLE: &str = "Bundle carries a Qobuz login";
pub const B_IMPORT_AUTH_BODY: &str =
    "Also log in with the bundled account? The token is validated with Qobuz\nbefore anything is stored.";
pub const B_IMPORT_AUTH_HINT: &str = "y log in · Esc skip auth";

/// Export include-auth warning (§3.6, shown while the toggle is on).
pub const B_EXPORT_AUTH_WARNING: &str = "embeds your decrypted Qobuz token — anyone with this file can use your\naccount. File is written 0600; move it privately (scp), delete after import.";

pub fn b_export_success(path: &str) -> String {
    format!("saved. on the daemon box: qbzd settings import {path}")
}
/// Success-panel hint when a desktop profile is detected (§3.6): desktop export
/// is the CLI's job.
pub const B_DESKTOP_HINT: &str =
    "a desktop QBZ profile was found on this box — to export IT instead:\n  qbzd settings export --from desktop";

pub fn b_import_done(applied: usize, adapted: usize, skipped: usize) -> String {
    format!("imported: {applied} applied, {adapted} adapted, {skipped} skipped")
}

// ============================ save result (§4.3) ============================

pub const SAVE_TITLE: &str = "Saved";
pub const RESULT_HINT: &str = "Enter / Esc close";

pub const SAVED_DISK_ONLY: &str =
    "saved to disk — daemon didn't answer; changes apply on restart";
pub const RELOAD_REFUSED: &str =
    "saved to disk — daemon answered but refused the reload; restart it:\n  systemctl --user restart qbzd";

// ============================ HiFi Wizard (FB4, §7) ============================

pub const WIZARD_TITLE: &str = "Wizard";

// Step names (breadcrumb `Wizard › <step>`).
pub const WIZ_STEP_WELCOME: &str = "Welcome";
pub const WIZ_STEP_CHECK: &str = "Check";
pub const WIZ_STEP_SELECT: &str = "Select DACs";
pub const WIZ_STEP_REVIEW: &str = "Review";
pub const WIZ_STEP_TEST: &str = "Test";
pub const WIZ_STEP_DONE: &str = "Done";

// Per-step help bars.
pub const WIZ_HELP_WELCOME: &str = "Enter start · → next · Tab nav · q quit";
pub const WIZ_HELP_CHECK: &str = "up/down move · Enter override · → next · ← back · Esc quit wizard";
pub const WIZ_HELP_SELECT: &str = "up/down move · Space toggle · m manual · → next · ← back · Esc quit";
pub const WIZ_HELP_REVIEW: &str =
    "up/down block · c copy · C copy all · w save · PgUp/PgDn scroll · → next · ← back";
pub const WIZ_HELP_TEST: &str = "t play test · r re-read · → next (skip) · ← back · Esc quit wizard";
pub const WIZ_HELP_DONE: &str = "Enter finish · ← back · Esc close";

// Welcome step.
pub const WIZ_WELCOME_TITLE: &str = "HiFi / DAC Setup Wizard";
pub const WIZ_WELCOME_BODY: &str = "This wizard checks your PipeWire/ALSA audio stack, finds your DAC(s), and\ngenerates the exact bit-perfect config for each one. It never touches a system\nfile — you copy the blocks and apply them yourself.\n\nSteps: Check the stack · Select DACs · Review the config · Test playback.";
pub const WIZ_WELCOME_CTA: &str = "Enter start";

// Check step.
pub const WIZ_DISTRO: &str = "Distribution";
pub const WIZ_INIT: &str = "Init system";
pub const WIZ_HEALTH_CHECKING: &str = "checking your audio stack…";
pub const WIZ_HEALTH_READY: &str = "✓ your audio stack is ready for bit-perfect playback";
pub const WIZ_HEALTH_ATTENTION: &str = "! some pieces need attention before bit-perfect playback will work:";
pub const WIZ_NO_REMEDIATION: &str = "nothing to change — the commands below are for reference only.";

pub fn wiz_sandbox_note(name: &str) -> String {
    format!(
        "running inside {name} — the host audio stack can't be probed from here; \
the commands below are the reference setup for the distro/init you pick."
    )
}

// Select-DACs step.
pub const WIZ_SELECT_INTRO: &str = "Detected outputs — check the DAC(s) you want bit-perfect config for:";
pub const WIZ_DETECTING: &str = "detecting DACs…";
pub const WIZ_DAC_BADGE: &str = "  [likely DAC]";
pub const WIZ_DEFAULT_BADGE: &str = "  [default]";
pub const WIZ_NO_DACS: &str = "no outputs enumerated — is PipeWire running and pw-dump installed?\nyou can still enter a node.name manually with 'm'.";
pub const WIZ_MANUAL_HINT: &str = "m — enter a PipeWire node.name manually (alsa_output.* / alsa_input.*)";
pub const WIZ_MANUAL_ACCEPTED: &str = "manual node:";
pub const WIZ_MANUAL_TITLE: &str = "Manual node.name";
pub const WIZ_MANUAL_BODY: &str = "Paste a PipeWire node.name (must contain alsa_output or alsa_input):";
pub const WIZ_MANUAL_INVALID: &str = "not a valid node.name — it must contain alsa_output or alsa_input";
pub const WIZ_SELECT_GATE: &str = "select at least one DAC (or enter a node.name with 'm') before continuing";

// Review step.
pub const WIZ_GENERATING: &str = "generating per-DAC config…";
pub const WIZ_BACKUP_HINT: &str = "tip: back up ~/.config/pipewire + ~/.config/wireplumber before applying anything.";
pub const WIZ_REVIEW_FOOTER: &str =
    "the wizard NEVER writes these files — copy (c/C) or save (w), then apply them yourself";
pub const WIZ_SAVED_TO: &str = "saved to";
pub const WIZ_SAVE_FAILED: &str = "could not save";

pub fn wiz_copied_all(n: usize) -> String {
    if n == 1 {
        "copied 1 block".to_string()
    } else {
        format!("copied all {n} blocks")
    }
}

// Test step.
pub const WIZ_TEST_INTRO: &str = "Play a track through the daemon, then read the DAC's REAL negotiated rate back\n(from /proc/asound) — the requested vs negotiated rate is the bit-perfect proof.";
pub const WIZ_TEST_NOTHING: &str = "nothing playing yet — press t to start the current queue";
pub const WIZ_TEST_WAITING: &str = "waiting for the DAC to open a stream…";
pub const WIZ_TEST_MATCHED: &str = "✓ the DAC clock matches what QBZ requested — bit-perfect";
pub const WIZ_TEST_REFERENCE: &str = "known reference track:";
pub const WIZ_TEST_SEEDS_HEADER: &str = "reference tracks you can cast/queue to verify each rate:";

// Done step.
pub const WIZ_DONE_TITLE: &str = "All set";
pub const WIZ_DONE_REMINDER: &str = "reminder: QBZ never writes system audio configs. Apply the blocks you copied,\nthen restart your PipeWire/WirePlumber user services (or log out and back in).";
pub const WIZ_DONE_RESTART: &str = "on this box, that is:";
pub const WIZ_DONE_CTA: &str = "Enter finish";

pub fn wiz_done_summary(dacs: usize) -> String {
    match dacs {
        0 => "No DAC config was generated — re-run the wizard to select a DAC.".to_string(),
        1 => "Generated bit-perfect config for 1 DAC.".to_string(),
        n => format!("Generated bit-perfect config for {n} DACs."),
    }
}

// Confirm-abandon modal (Esc mid-wizard).
pub const WIZ_ABANDON_TITLE: &str = "Quit the wizard?";
pub const WIZ_ABANDON_BODY: &str = "Your selections and generated config will be discarded.";
pub const WIZ_ABANDON_HINT: &str = "y quit · Esc stay";

// ============================ Scrobbler (CONSOLE ext) ============================

pub const SCROBBLER_TITLE: &str = "Scrobbler";
pub const HELP_SCROBBLER: &str = "L connect Last.fm · B connect ListenBrainz · Tab nav · Esc nav · q quit";
// Alt-screen handoffs — printed on the plain terminal before the CLI auth flow
// runs (same methodology as the Account browser login).
pub const SCROBBLE_LASTFM_HANDOFF: &str =
    "Connecting Last.fm — a browser authorize step follows below.\n";
pub const SCROBBLE_LISTENBRAINZ_HANDOFF: &str =
    "Connecting ListenBrainz — paste your user token below.\n";
pub const SCROBBLE_RETURN_HINT: &str = "\nPress Enter to return to setup…";
