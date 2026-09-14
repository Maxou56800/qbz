#!/usr/bin/env bash
# qbzd P0 acceptance -- scripted checks (05-implementation-plan.md T16).
#
# The human-only steps (J1 fresh-Pi journey, J2 desktop-handoff, the >24h JWT
# soak, and the sign-off sheet) live in
# qbz-nix-docs/qbz-daemon/acceptance/P0-acceptance.md. This script is the
# automatable slice only.
#
# SAFETY: runs the PREBUILT `qbzd` binary -- it never invokes cargo/rustc, so
# it is safe to run on a box where a build is already in flight elsewhere
# (global constraint: one build at a time, box-wide). It boots the daemon
# against an ISOLATED scratch profile root (env-driven XDG_CONFIG_HOME /
# XDG_DATA_HOME / XDG_CACHE_HOME) on a non-default port -- it never reads or
# writes the real ~/.config/qbzd, ~/.local/share/qbzd or any desktop qbz
# profile, and never touches systemd user units. Safe to run next to a real
# qbzd/qbz install.
#
# Usage:
#   ./scripts/qbzd-acceptance.sh
#   QBZD_BIN=/path/to/qbzd ./scripts/qbzd-acceptance.sh
#   QBZD_TEST_PORT=28182 ./scripts/qbzd-acceptance.sh
#   QBZD_KEEP_SCRATCH=1 ./scripts/qbzd-acceptance.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
QBZD_BIN="${QBZD_BIN:-$ROOT/crates/target/release/qbzd}"
PORT="${QBZD_TEST_PORT:-28182}"

fail() { echo "FAIL: $1" >&2; exit 1; }

command -v curl    >/dev/null 2>&1 || fail "curl is required"
command -v python3 >/dev/null 2>&1 || fail "python3 is required (status/ping/info shape checks)"
command -v timeout  >/dev/null 2>&1 || fail "timeout (coreutils) is required"
[ -x "$QBZD_BIN" ] || fail "qbzd binary not found/executable at $QBZD_BIN -- build it first (release, on its own, into this worktree's target: python3 scripts/qt-cargo.py build --release --manifest-path crates/Cargo.toml -p qbzd)"

# ---------------------------------------------------------------------------
# Isolated scratch profile root. NEVER the real daemon/desktop roots: dirs::
# config_dir()/data_dir()/cache_dir() (crates/qbzd/src/paths.rs) honor these
# three env vars on Linux, and every qbzd invocation (daemon and CLI alike)
# resolves its roots this same way (main.rs: ProfileRoots::resolve(None, None)
# everywhere) -- so exporting them here is sufficient isolation for the whole
# script, with no --config/--data-root flag needed.
# ---------------------------------------------------------------------------
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/qbzd-acceptance.XXXXXX")"
export XDG_CONFIG_HOME="$SCRATCH/config"
export XDG_DATA_HOME="$SCRATCH/data"
export XDG_CACHE_HOME="$SCRATCH/cache"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"

# Belt-and-braces: refuse to run if isolation somehow didn't take.
case "$XDG_CONFIG_HOME" in
  "$HOME"/.config|"$HOME"/.config/*) fail "refusing to run: XDG_CONFIG_HOME resolved under \$HOME/.config -- isolation broke" ;;
esac

QBZD_HOST="127.0.0.1:$PORT"
LOGFILE="$SCRATCH/qbzd.log"
DAEMON_PID=""

# Escalating kill helper: SIGTERM → poll 5s → SIGKILL → poll 2s.
# Returns 0 if confirmed dead, 1 if still alive after SIGKILL.
# If still alive after SIGKILL, prints a warning with the PID.
kill_and_confirm() {
  local pid=$1
  [ -n "$pid" ] || return 0

  # SIGTERM and poll for up to 5 seconds (25 x 0.2s).
  kill -TERM "$pid" 2>/dev/null || true
  for _ in $(seq 1 25); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.2; done

  # Still alive; escalate to SIGKILL and poll for up to 2 seconds (10 x 0.2s).
  kill -KILL "$pid" 2>/dev/null || true
  for _ in $(seq 1 10); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.2; done

  # Still alive after SIGKILL; print warning and return 1.
  echo "WARNING: qbzd (PID $pid) still alive after SIGKILL" >&2
  return 1
}

cleanup() {
  local exit_status=$?
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill_and_confirm "$DAEMON_PID" || true
  fi
  if [ "$exit_status" -ne 0 ] || [ "${QBZD_KEEP_SCRATCH:-0}" = "1" ]; then
    echo "qbzd acceptance scratch preserved at: $SCRATCH" >&2
  else
    rm -rf "$SCRATCH"
  fi
  return "$exit_status"
}
trap cleanup EXIT

# Never steal a live port -- if something already answers, stop rather than guess.
if curl -fsS -m 1 "127.0.0.1:${PORT}/api/ping" >/dev/null 2>&1; then
  fail "something is already answering on 127.0.0.1:${PORT} -- set QBZD_TEST_PORT to a free port"
fi

qbzd() { "$QBZD_BIN" --host "$QBZD_HOST" "$@"; }

write_config() {
  mkdir -p "$XDG_CONFIG_HOME/qbzd"
  # Pins the test port (never 8182 -- avoids colliding with a real daemon) and
  # plants one unrecognized key so the 01 §10.2 unknown-key startup warning is
  # exercised on every boot this script does.
  cat > "$XDG_CONFIG_HOME/qbzd/qbzd.toml" <<EOF
config_version = 1
acceptance_script_marker = "unknown key on purpose (01 section 10.2)"

[server]
bind = "127.0.0.1"
port = $PORT
EOF
}

start_daemon() {
  write_config
  : > "$LOGFILE"
  "$QBZD_BIN" run >>"$LOGFILE" 2>&1 &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do
    qbzd ping >/dev/null 2>&1 && return 0
    kill -0 "$DAEMON_PID" 2>/dev/null || fail "daemon exited during boot -- see $LOGFILE"
    sleep 0.2
  done
  fail "daemon did not answer ping within 10s -- see $LOGFILE"
}

stop_daemon() {
  [ -n "$DAEMON_PID" ] || return 0
  if kill_and_confirm "$DAEMON_PID"; then
    DAEMON_PID=""
  else
    # Process still alive after escalation; leave DAEMON_PID set so cleanup() gets another chance.
    true
  fi
}

echo "== isolated-root boot (env-driven scratch XDG) =="
start_daemon
echo "  scratch root: $SCRATCH"

echo "== exit-code table (02 section 1.3): version ok, unknown verb is usage error =="
qbzd version >/dev/null || fail "version != 0"
set +e
qbzd bogus-command-xyz >/dev/null 2>&1
rc_usage=$?
set -e
[ "$rc_usage" -eq 2 ] || fail "usage error != 2 (got $rc_usage)"

echo "== unknown-key config warning (01 section 10.2) =="
grep -q 'unknown key: acceptance_script_marker' "$LOGFILE" \
  || fail "boot did not warn about the unrecognized qbzd.toml key -- see $LOGFILE"

echo "== status answers 'why is it silent' in one call (02 section 3.3.3) =="
# This scratch daemon never logs in, so status is expected to report
# auth.state=needs_auth and the CLI exits 4 (02 section 1.3: "status exits
# nonzero on degraded state") -- that is the down-vs-unhealthy distinction
# working correctly, not a script failure, so capture rc separately from
# `set -e` and assert the SPECIFIC expected code rather than just >/dev/null.
set +e
status_json=$(qbzd status --json)
rc_status_needs_auth=$?
set -e
[ "$rc_status_needs_auth" -eq 4 ] || fail "status --json on an unauthenticated daemon != 4 (got $rc_status_needs_auth)"
echo "$status_json" | python3 -c "
import json, sys
d = json.load(sys.stdin)
for k in ('auth', 'audio', 'playback', 'qconnect', 'network', 'last_errors', 'driver_tick_age_ms'):
    assert k in d, f'missing status key: {k}'
assert d['auth']['state'] == 'needs_auth', d['auth']
"

echo "== ping/info shape (02 section 3.3.1 / 3.3.2) =="
curl -fsS "127.0.0.1:${PORT}/api/ping" | python3 -c "
import json, sys
d = json.load(sys.stdin)
assert d.get('ok') is True, d
assert d.get('app') == 'qbzd', d
assert 'api_version' in d, d
"
curl -fsS "127.0.0.1:${PORT}/api/info" | python3 -c "
import json, sys
d = json.load(sys.stdin)
for k in ('app', 'version', 'api_version', 'bind', 'uptime_secs', 'data_root'):
    assert k in d, f'missing info key: {k}'
"

echo "== config show --json matches the on-disk port (02 section 2.2 config verb) =="
cfg_port=$(qbzd config show --json | python3 -c "import json,sys; print(json.load(sys.stdin)['server']['port'])")
[ "$cfg_port" = "$PORT" ] || fail "config show --json port ($cfg_port) != expected test port ($PORT)"

echo "== export/import roundtrip is a no-op (04 section 7) =="
# The 04 section 7 roundtrip guarantee assumes a daemon "legitimately running"
# its own settings -- concretely, one that has already moved past the fresh
# store's quality_fallback_behavior=ask default (the TUI never writes "ask",
# 03 section 3.3.2; "settings set" rejects it outright, cli/settings.rs). A
# virgin store that never ran `qbzd setup`/`settings set` still holds "ask",
# which the section 5.5 mapping unconditionally reports as `adapted` -- so
# prime it first, exactly like a real first-run setup would, before proving
# the no-op invariant.
qbzd settings set audio.quality_fallback_behavior always_fallback >/dev/null
BUNDLE="$SCRATCH/rt.qbzb"
qbzd settings export "$BUNDLE" >/dev/null
out=$(qbzd settings import "$BUNDLE" --dry-run)
echo "$out" | grep -q "adapted (0)" || fail "roundtrip produced adaptations (no-change short-circuit broken -- 04 section 5.3 step 4)"
echo "$out" | grep -qE '^applied \([0-9]+\)' || fail "roundtrip import produced no applied-bucket summary"
rm -f "$BUNDLE"

echo "== route budget: unknown route 404s, ping open, Origin rejected (02 section 3.1.2 / 3.1.4) =="
curl -fsS "127.0.0.1:${PORT}/api/ping" >/dev/null || fail "open ping"
code=$(curl -s -o /dev/null -w '%{http_code}' "127.0.0.1:${PORT}/api/nope")
[ "$code" = "404" ] || fail "unknown route: $code"
ocode=$(curl -s -o /dev/null -w '%{http_code}' -H 'Origin: http://x' "127.0.0.1:${PORT}/api/status")
[ "$ocode" = "403" ] || fail "origin shield: $ocode"

echo "== daemon-down: ping/status exit 3 (02 section 1.3) =="
stop_daemon
set +e
qbzd ping >/dev/null 2>&1
rc_ping=$?
qbzd status >/dev/null 2>&1
rc_status=$?
set -e
[ "$rc_ping" -eq 3 ]   || fail "ping daemon-down != 3 (got $rc_ping)"
[ "$rc_status" -eq 3 ] || fail "status daemon-down != 3 (got $rc_status)"

echo "== instance lock: a second 'qbzd run' on the same root exits 3 (01 section 8.3) =="
start_daemon
set +e
timeout 5 "$QBZD_BIN" run >>"$LOGFILE" 2>&1
rc_second=$?
set -e
[ "$rc_second" -eq 3 ] || fail "second 'qbzd run' on the same data root != 3 (got $rc_second)"
qbzd ping >/dev/null || fail "the first daemon stopped answering after the double-start attempt"

echo "== non-tty 'qbzd setup' exits 2, never hangs (03 section 2.4) =="
set +e
timeout 5 "$QBZD_BIN" setup </dev/null >/dev/null 2>&1
rc_setup=$?
set -e
[ "$rc_setup" -eq 2 ] || fail "non-tty setup != 2 (got $rc_setup)"

stop_daemon
echo "ALL SCRIPTED CHECKS PASSED"
