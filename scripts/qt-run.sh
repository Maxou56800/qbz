#!/usr/bin/env bash
# QBZ Qt/QML — build with cargo, then run the BINARY DIRECTLY.
#
# The Qt-track sibling of slint-run.sh. Same shape (build, learned ETA, exec the
# prebuilt binary so process monitors show `qbz-qt` instead of `cargo`), but the
# constraints are almost the opposite — read the two sections below before
# "improving" this script into a copy of slint-run.sh.
#
# ─── THERE IS NO MEMORY WALL HERE ───────────────────────────────────────────
# slint-run.sh tiers threads/codegen-units/opt-level off MemAvailable and runs
# under a cgroup cap because ONE rustc for `qbz-ui` (the ~1.6 M-line generated
# Slint module) peaks 20-30 GB and can freeze the box.
# `qbz-qt` does NOT depend on `qbz-ui` — verified: zero `qbz-ui` references in
# crates/qbz-qt/Cargo.toml, so the monster compilation unit never enters this
# graph. The UI lives in QML, which does not go through rustc at all; that is
# the entire point of the Qt track. So: no tiering, no cap, ordinary jobs.
# The box-wide "ONE build at a time" rule still applies (a Slint build running
# in another worktree WILL freeze the machine) — hence the guard below.
#
# ─── THE CACHE RULE — WHY THIS SCRIPT SETS NO RUSTFLAGS ─────────────────────
# RUSTFLAGS is part of every unit's fingerprint. This worktree's target dir was
# built on STABLE with rustflags=[] (checked in the .fingerprint json). Exporting
# anything — mold, `-Z threads`, a target-feature — invalidates the whole
# workspace and buys a from-scratch rebuild of ~200 crates to save seconds on a
# ~2-minute incremental. Same reason there is no `+nightly` here: nothing in
# qbz-qt needs it, and switching toolchains is another full rebuild.
# MOLD=1 opts in anyway (it warns first). If you use it, keep using it.
#
# ─── THE QML AUDITS RUN FIRST, BY DESIGN ────────────────────────────────────
# `cargo check` sees NOTHING of the QML: a missing component or a call to a
# bridge member that does not exist compiles clean and throws when the user
# opens that view. The five static audits are ~1s and catch exactly that, so they
# run BEFORE the build — failing in one second beats failing after two minutes,
# or worse, at the owner's smoke. NO_AUDIT=1 skips them.
#
# ─── EACH WORKTREE BUILDS IN ITS OWN TARGET ─────────────────────────────────
# scripts/qt-target.py decides where. With Cargo's default target (inside the
# checkout: macOS, CI) nothing changes. When the host configures ONE target
# outside the checkout for every worktree, this worktree builds in its own
# sibling of it, `<target>-worktrees/<worktree>`, so a build in another worktree
# can never replace the binary built here; rebuilding THIS worktree still does.
# Its first build of a profile is seeded from the shared target with
# copy-on-write clones, and crates/target is re-pointed after each successful
# build. An explicit CARGO_TARGET_DIR is used exactly as given.
#
# Usage: ./scripts/qt-run.sh [extra app args]
#   DEBUG=1     ./scripts/qt-run.sh   # debug profile (the binary the gate uses)
#   NORUN=1     ./scripts/qt-run.sh   # build only, don't exec
#   TEST=1      ./scripts/qt-run.sh   # also run `cargo test -p qbz-qt`
#   SMOKE=1     ./scripts/qt-run.sh   # offscreen gate instead of the GUI
#   NO_AUDIT=1  ./scripts/qt-run.sh   # skip the five QML audits
#   JOBS=4      ./scripts/qt-run.sh   # cargo build jobs (default: auto)
#   MOLD=1      ./scripts/qt-run.sh   # link with mold (forces a full rebuild)
#   FORCE=1     ./scripts/qt-run.sh   # build even if another cargo/rustc is up
#   NO_TICKER=1 ./scripts/qt-run.sh   # no live progress ticker
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

QT_CRATE="crates/qbz-qt"
# The five QML audits ship IN the repo (scripts/qml-audits, 2026-08-25) so CI
# runs the same gate as this script; AUDIT_DIR still overrides, and the docs
# checkout is the fallback for a tree that predates the move.
AUDIT_DIR="${AUDIT_DIR:-scripts/qml-audits}"
[[ -d "$AUDIT_DIR" ]] || AUDIT_DIR="$HOME/Personal/qbz/qbz-nix-docs/qt-frontend/tools"

# --- Portability shims (this runs on the Mac mini too) -----------------------
# Everything below that differs between GNU/Linux and BSD/macOS lives here, so
# the body reads the same on both. On macOS the audits also live elsewhere (the
# docs repo is not synced to the build box), so a missing AUDIT_DIR degrades to
# a warning rather than an error — see the audit block.
UNAME_S="$(uname -s)"
# Available RAM in MB. macOS has no `free`; vm_stat's "free + inactive" pages
# are the honest analogue of MemAvailable (inactive is reclaimable).
mem_avail_mb() {
  if command -v free >/dev/null 2>&1; then
    free -m | awk '/^Mem:/ {print $7}'
  elif [[ "${UNAME_S}" == "Darwin" ]]; then
    vm_stat | awk '/page size of/ {ps=$8} /Pages free/ {f=$3} /Pages inactive/ {i=$3}
                   END { gsub(/\./,"",f); gsub(/\./,"",i); if (ps=="") ps=16384;
                         printf "%d", (f+i)*ps/1048576 }'
  else
    echo 0
  fi
}
cpu_count() {
  if command -v nproc >/dev/null 2>&1; then nproc
  elif [[ "${UNAME_S}" == "Darwin" ]]; then sysctl -n hw.ncpu
  else echo 4; fi
}
# BSD pgrep has no -c and uses -l where GNU uses -a, so count lines instead.
# The `|| true` is load-bearing: pgrep exits 1 when it matches nothing, which is
# the NORMAL case here, and under `set -euo pipefail` that non-zero status
# propagates out of the command substitution and kills the script — silently,
# before a single line of output, looking exactly like a build that did nothing.
# (It did exactly that once. The tell was an empty log with rc=1.)
other_builds() { { pgrep -x rustc; pgrep -x cargo; } 2>/dev/null || true; }
# `timeout` is GNU coreutils and is simply ABSENT on macOS, where the shell
# answers `command not found` — which, inside the smoke block, produced a
# one-line log that then reported "0 QML complaints" and looked like a pass.
# A smoke that never started must never read as green, so this shims it.
run_timeout() {
  local secs=$1; shift
  if command -v timeout >/dev/null 2>&1; then timeout "${secs}" "$@"; return $?; fi
  if command -v gtimeout >/dev/null 2>&1; then gtimeout "${secs}" "$@"; return $?; fi
  "$@" & local pid=$!
  ( sleep "${secs}"; kill "${pid}" 2>/dev/null ) & local watcher=$!
  wait "${pid}" 2>/dev/null; local rc=$?
  kill "${watcher}" 2>/dev/null || true
  return "${rc}"
}

# cxx-qt-build finds Qt through qmake / CMAKE_PREFIX_PATH. Homebrew keeps it
# off PATH by design (qt is keg-only-ish in practice), so a plain shell has no
# qmake and the build dies in build.rs rather than in the compiler. Add it when
# it is there; say nothing when it is not, so Linux is untouched.
if [[ "${UNAME_S}" == "Darwin" ]]; then
  for qtdir in /opt/homebrew/opt/qt /usr/local/opt/qt; do
    if [[ -x "${qtdir}/bin/qmake" ]]; then
      export PATH="${qtdir}/bin:${PATH}"
      export CMAKE_PREFIX_PATH="${qtdir}${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}"
      break
    fi
  done
fi

# --- Pretty helpers ----------------------------------------------------------
if [[ -t 2 ]]; then C_DIM=$'\033[2m'; C_BOLD=$'\033[1m'; C_GRN=$'\033[32m'
  C_RED=$'\033[31m'; C_YEL=$'\033[33m'; C_RST=$'\033[0m'
else C_DIM=""; C_BOLD=""; C_GRN=""; C_RED=""; C_YEL=""; C_RST=""; fi
fmt_dur() { local s=$1; printf '%dm %02ds' $(( s / 60 )) $(( s % 60 )); }
say()  { printf '[qt-run] %s\n' "$*"; }
warn() { printf '%s[qt-run] %s%s\n' "${C_YEL}" "$*" "${C_RST}" >&2; }
die()  { printf '%s[qt-run] %s%s\n' "${C_RED}" "$*" "${C_RST}" >&2; exit 1; }

if [[ "${DEBUG:-0}" == 1 ]]; then
  PROFILE=debug;   PROFILE_ARGS=()
else
  PROFILE=release; PROFILE_ARGS=(--release)
fi

# --- ONE BUILD AT A TIME, BOX-WIDE ------------------------------------------
# Not paranoia: a Slint `qbz-ui` rustc in another worktree peaks 20-30 GB on a
# 30 GB box with no hibernation, and the failure mode is a swap-thrash livelock
# that needs a power-cycle — earlyoom does not reliably fire. Cheap to check.
if [[ "${FORCE:-0}" != 1 ]]; then
  others=$(other_builds | wc -l | tr -d ' ')
  [[ "${others}" =~ ^[0-9]+$ ]] || others=0
  if (( others > 0 )); then
    die "another cargo/rustc is running (${others}). Builds are serialized box-wide — wait, or FORCE=1 if you know it is harmless."
  fi
fi

# --- Where this worktree builds (see the header) -----------------------------
# The PHYSICAL path: moc/cc-rs resolve includes wrongly through the crates/target
# symlink.
CARGO_TARGET_DIR="$(python3 scripts/qt-target.py resolve)" || die "could not resolve the Cargo target directory"
export CARGO_TARGET_DIR
BIN="${CARGO_TARGET_DIR}/${PROFILE}/qbz"

# --- The QML audits (fail fast; cargo cannot see QML) ------------------------
if [[ "${NO_AUDIT:-0}" != 1 ]]; then
  if [[ -d "${AUDIT_DIR}" ]] && command -v python3 >/dev/null 2>&1; then
    audit_abs="$(pwd)/${QT_CRATE}"
    # `qml_module_registration_audit.py` is the newest and it exists because the
    # other three could not see the failure it catches: a .qml on disk that is
    # missing from build.rs is not in the QML MODULE, so it resolves fine on
    # disk, compiles nothing, and blanks a whole page at runtime with
    # "<Component> is not a type" (2026-08-20, the media-server panel).
    # qml_icon_bake_audit.py joined on 2026-08-20: QbzIcon draws PRE-BAKED
    # per-tint svgs, so an icon name with no bake renders NOTHING — no error,
    # no failing test, no complaint from the other four. Three menu rows
    # shipped iconless through a green build before anyone looked at the menu.
    # qml_on_prefixed_property_audit.py joined on 2026-09-01: a property named
    # `on<Upper>` is filed as a signal handler as soon as a sibling member
    # `<upper>` exists, its initializer never binds and it sits at the type
    # default with no error — `onAlbums` next to `albums` blanked Purchases.
    for a in qml_resolution_audit.py qml_singleton_xref.py qml_eager_tab_audit.py qml_module_registration_audit.py qml_icon_bake_audit.py qml_on_prefixed_property_audit.py; do
      [[ -r "${AUDIT_DIR}/${a}" ]] || { warn "audit ${a} not found — skipped"; continue; }
      if ! python3 "${AUDIT_DIR}/${a}" "${audit_abs}"; then
        die "${a} FAILED — fix it before burning a build (QML resolves lazily; this is what cargo cannot tell you)."
      fi
    done
    say "QML audits OK"
  else
    warn "audit tools not found at ${AUDIT_DIR} — skipped"
  fi
fi

# --- Build settings ----------------------------------------------------------
# Deliberately NOT setting CARGO_PROFILE_RELEASE_* (opt-level / codegen-units):
# they are fingerprint inputs too, and unlike the Slint track there is no memory
# reason to lower them. The package's own [profile.release] (strip="symbols")
# is the whole story.
if [[ -n "${JOBS:-}" ]]; then
  export CARGO_BUILD_JOBS="${JOBS}"
else
  # Leave a couple of cores for the desktop; cargo's default is all of them.
  ncpu=$(cpu_count)
  export CARGO_BUILD_JOBS="$(( ncpu > 3 ? ncpu - 2 : 1 ))"
fi

if [[ "${MOLD:-0}" == 1 ]]; then
  command -v mold >/dev/null 2>&1 || die "MOLD=1 but mold is not installed"
  warn "MOLD=1 changes RUSTFLAGS, which is a fingerprint input → this rebuilds the WHOLE workspace once. Keep using it, or never use it."
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi

# --- Learned ETA: last successful build duration for THIS profile ------------
eta_dir="${XDG_CACHE_HOME:-$HOME/.cache}/qbz-qt"
eta_file="${eta_dir}/last-build-${PROFILE}.secs"
eta_secs=0
[[ -r "${eta_file}" ]] && eta_secs=$(cat "${eta_file}" 2>/dev/null || echo 0)
[[ "${eta_secs}" =~ ^[0-9]+$ ]] || eta_secs=0

avail_mb=$(mem_avail_mb)
say "profile=${PROFILE} jobs=${CARGO_BUILD_JOBS} avail=${avail_mb}MB rustflags='${RUSTFLAGS:-}'"
say "target=${CARGO_TARGET_DIR}"

# A worktree target's first build of a profile starts from the shared target's
# artifacts (copy-on-write, nothing duplicated) instead of from nothing. After
# the one-build guard on purpose: never clone a target another build is writing.
seed_profiles=("${PROFILE}")
[[ "${TEST:-0}" == 1 && "${PROFILE}" != debug ]] && seed_profiles+=(debug)
if [[ -z "$(other_builds)" ]]; then
  python3 scripts/qt-target.py seed --target-dir "${CARGO_TARGET_DIR}" "${seed_profiles[@]}" \
    || warn "could not seed ${CARGO_TARGET_DIR}; Cargo builds it from scratch"
fi

# --- Start banner ------------------------------------------------------------
build_start=$(date +%s)
if (( eta_secs > 0 )); then eta_txt="~$(fmt_dur "${eta_secs}") (last ${PROFILE})"
else eta_txt="unknown (first ${PROFILE} build)"; fi
printf '%s[qt-run] ▶ build started %s  ·  ETA %s%s\n' \
  "${C_BOLD}" "$(date '+%H:%M:%S')" "${eta_txt}" "${C_RST}"

# --- Live ticker -------------------------------------------------------------
# 30s, not slint-run's 15s: a warm qbz-qt build is ~2 min, so 15s would print
# more ticker than build. A cold one (or the first after `cargo clean`) is long
# enough that the ticker earns its place.
tick_pid=""
if [[ "${NO_TICKER:-0}" != 1 ]] && [[ -t 2 ]]; then
  (
    while true; do
      sleep 30
      now=$(date +%s); el=$(( now - build_start ))
      if (( eta_secs > 0 )); then
        pct=$(( el * 100 / eta_secs )); (( pct > 99 )) && pct=99
        printf '%s[qt-run] ⏱  %s elapsed · ETA ~%s · ~%d%%%s\n' \
          "${C_DIM}" "$(fmt_dur "${el}")" "$(fmt_dur "${eta_secs}")" "${pct}" "${C_RST}" >&2
      else
        printf '%s[qt-run] ⏱  %s elapsed%s\n' "${C_DIM}" "$(fmt_dur "${el}")" "${C_RST}" >&2
      fi
    done
  ) &
  tick_pid=$!
  trap '[[ -n "${tick_pid}" ]] && kill "${tick_pid}" 2>/dev/null || true' EXIT
fi

# --- The build ---------------------------------------------------------------
python3 scripts/qt-cargo.py build "${PROFILE_ARGS[@]}" --manifest-path crates/Cargo.toml -p qbz-qt

# The familiar crates/target path follows the target this worktree just built,
# and only after a successful build.
python3 scripts/qt-target.py link --target-dir "${CARGO_TARGET_DIR}" \
  || warn "crates/target was not re-pointed; the binary is ${BIN}"

# --- Stop the ticker, record the duration, print the final banner ------------
[[ -n "${tick_pid}" ]] && { kill "${tick_pid}" 2>/dev/null || true; wait "${tick_pid}" 2>/dev/null || true; }
trap - EXIT
build_secs=$(( $(date +%s) - build_start ))
mkdir -p "${eta_dir}" 2>/dev/null && printf '%s\n' "${build_secs}" > "${eta_file}" 2>/dev/null || true
printf '%s[qt-run] ✔ build finished %s  ·  took %s  (%s)%s\n' \
  "${C_BOLD}${C_GRN}" "$(date '+%H:%M:%S')" "$(fmt_dur "${build_secs}")" "${PROFILE}" "${C_RST}"
say "binary=${BIN}"

# --- Tests (opt-in) ----------------------------------------------------------
if [[ "${TEST:-0}" == 1 ]]; then
  say "running cargo test -p qbz-qt"
  python3 scripts/qt-cargo.py test --manifest-path crates/Cargo.toml -p qbz-qt
fi

# --- Offscreen smoke gate (opt-in) -------------------------------------------
# The last step of the track's standard gate: boot the real app with no display
# and prove the QML tree actually resolves. A lazily-resolved type error only
# ever shows up here or in front of the owner.
if [[ "${SMOKE:-0}" == 1 ]]; then
  if [[ "${UNAME_S}" == "Linux" ]]; then
    log="$(mktemp "${TMPDIR:-/tmp}/qbz-qt-smoke-XXXXXX")"
    python3 scripts/qt-smoke.py "${BIN}" --log "${log}"
    exit 0
  fi
  # BSD mktemp -t takes a bare prefix, GNU takes a template; give both a full
  # template path so the two agree.
  log="$(mktemp "${TMPDIR:-/tmp}/qbz-qt-smoke-XXXXXX")"
  say "offscreen smoke (75s max) → ${log}"
  QT_QPA_PLATFORM=offscreen RUST_LOG=info run_timeout 75 "${BIN}" > "${log}" 2>&1 || true
  # A log this short means the process never really started (the macOS
  # `timeout`-not-found case did exactly that). Zero complaints in an empty log
  # is not a pass.
  if (( $(wc -l < "${log}") < 10 )); then
    cat "${log}" >&2
    die "smoke produced almost no output — the app did not start; see ${log}"
  fi
  # `propertyCache` from SettingsButton is known-benign noise — filter it out
  # rather than let it mask a real count.
  errs=$(grep -av 'propertyCache' "${log}" \
    | grep -aciE 'is not a type|unavailable|ReferenceError|TypeError|Cannot read|Unable to assign|Cannot open|no such method|has no' || true)
  published=$(grep -ac 'home published' "${log}" || true)
  say "smoke: qml complaints=${errs} (want 0) · 'home published'=${published} (want >=1)"
  if (( errs > 0 )); then
    grep -av 'propertyCache' "${log}" \
      | grep -aiE 'is not a type|unavailable|ReferenceError|TypeError|Cannot read|Unable to assign|Cannot open|no such method|has no' | head -20 >&2
    die "smoke FAILED — see ${log}"
  fi
  (( published >= 1 )) || die "smoke FAILED: the app never reached 'home published' — see ${log}"
  printf '%s[qt-run] ✔ smoke gate PASSED%s\n' "${C_BOLD}${C_GRN}" "${C_RST}"
  exit 0
fi

[[ "${NORUN:-0}" == 1 ]] && { say "build done (NORUN set)."; exit 0; }

# The Slint qbz holds the DAC exclusively — two players cannot open it at once.
if pgrep -x qbz >/dev/null 2>&1; then
  warn "the Slint 'qbz' is running — close it first or the audio device stays busy (ALSA exclusive)."
fi

# exec the binary directly — no `cargo run`, so no CARGO_* env / cargo context,
# so process monitors show `qbz-qt` rather than `cargo`.
exec "${BIN}" "$@"
