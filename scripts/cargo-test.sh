#!/usr/bin/env bash
# scripts/cargo-test.sh — the EXACT local mirror of .github/workflows/test-crates.yml.
#
# Rule (owner, 2026-08-26): test-crates is a living regression gate. Every
# step it gains lands here in the same commit, and vice versa; the workflow
# calls THIS script so the two cannot diverge. Bounded: --no-fail-fast and
# the `test` job's 20-minute limit. CARGO_BUILD_JOBS is left to cargo (= all
# cores): the old "2" was a Slint-era memory tier, and measured cold on the
# 4-vCPU CI VM (2026-08-26) jobs=4 took 392 s vs 589 s at jobs=2 with a
# 2.7 GB peak — nothing in this graph needs the throttle.
#
# Default (job `test`, no Qt):
#   cargo test --workspace --exclude qbz-qt
#   + the listen-log suite check: `qbz-app::listen_log` must list >= 25 tests
#     and pass (schema migration, accumulator: paused/seek/stall deltas add
#     nothing, natural/skip/stop/shutdown closes, orphan close on reopen,
#     paused writes nothing, clear + VACUUM, is_play/is_skip bounds).
#   + the DLNA device-description tolerance suite (qbz-cast): a renderer whose
#     SCPDURL/controlURL/eventSubURL is empty or relative must still parse.
#     The local rupnp compatibility patch must preserve current http (#745).
#   + the fatal-signal reporter check (qbz-log): a real child process is
#     segfaulted on purpose; the handler must name the signal and the process
#     must still die OF that signal.
#   --workspace = members of crates/Cargo.toml minus qbz-qt: the
#   audio/player/cache/DSD/disc/rip core, qbz-app/core/models/theme/i18n,
#   the Qobuz client and the source seam, Plex/Jellyfin/Subsonic + media
#   cache, local library + catalog, integrations/reco/lyrics/radio/mixtape/
#   playlist-import/media-controls/cast, credentials/secrets/offline-cache,
#   the qconnect-* crates, the HiFi wizard core, qbz-control and qbzd. Nothing here
#   needs Qt; wayland-sys enters the graph only through qbz-qt.
#   The Slint crates (qbz, qbz-ui, qbz-dac-wizard, qbz-slint-common) are gone
#   from the workspace: no exclusions for them, and never bring them back.
#
# QT=1 (jobs `shader-gate` + `qt-gate`, needs a Qt >= 6.8 install):
#   1. the five static QML audits (scripts/qml-audits)
#   2. the shader bake gate with the qsb on PATH (CI: the pinned aqt qsb)
#   3. the Slint-free dep-graph gate for qbz-qt
#   4. cargo test -p qbz-qt (debug)
#   5. offscreen boots of BOTH debug and release: zero QML complaints,
#      QbzCore initialized, and process still alive at the deadline.
#      Native Qt SDK content participates in the C++ dependency cache.
#   6. release xcb boot against a private silent D-Bus (requires Xvfb).
#   The shared runtime gate also executes Local Library QML logic and Kiosk artwork/navigation with Node.
#
# Usage:
#   ./scripts/cargo-test.sh                 # job `test`
#   ./scripts/cargo-test.sh -- --lib        # extra cargo test args
#   QT=1 ./scripts/cargo-test.sh            # the Qt gates too
#   QBZ_QT_QML_DIR=/path/to/qt/qml QT=1 ./scripts/cargo-test.sh   # point the audits at a Qt
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
say() { printf '[cargo-test] %s\n' "$*"; }

say "gate: native Qt SDK cache and crash-aware smoke regressions"
python3 scripts/test_qt_build_gates.py

say "gate: Windows app-local CRT import regressions"
python3 scripts/packaging/test_windows_crt.py

say "gate: packaged Qt process survival and smoke isolation"
python3 scripts/packaging/test_smoke_release.py

say "gate: all eight gettext catalogs"
for locale in en es de fr pt ru ja nl; do
  msgfmt --check --output-file=/dev/null "crates/qbz-i18n/translations/$locale/LC_MESSAGES/qbz-ui.po"
done

say "job test: cargo test --workspace --exclude qbz-qt (jobs=${CARGO_BUILD_JOBS:-all cores})"
cargo test \
  --manifest-path crates/Cargo.toml \
  --workspace \
  --exclude qbz-qt \
  --no-fail-fast \
  "$@"

say "gate: playback memory profiles, persistence and live growth"
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-models --lib -- --list profile_tests:: 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 3 ] || { say "FAIL: playback memory profile regressions missing"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-cache --lib -- --list memory_profile_tests:: 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 1 ] || { say "FAIL: live profile budget regression missing"; exit 1; }

say "gate: playback cache policy and streaming-promotion regressions"
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-cache --lib -- --list playback_policy_tests:: 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 8 ] || { say "FAIL: playback cache policy regressions missing ($n < 8)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-player --lib -- --list promotion_keeps_24bit_192khz 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 1 ] || { say "FAIL: Hi-Res promotion regression missing"; exit 1; }

say "gate: bounded disk playback regressions"
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-cache --lib -- --list disk_reader_tests:: 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 3 ] || { say "FAIL: disk reader/atomic replacement regressions missing"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-player --lib -- --list disk_ 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 5 ] || { say "FAIL: disk streaming/seek/cancellation regressions missing"; exit 1; }

say "gate: one-line installer (isolated fixture homes)"
python3 scripts/test-installer.py

say "gate: watcher recovery regressions"
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-library --lib -- --list watcher::tests:: 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 4 ] || { say "FAIL: watcher retry/recovery/read-feedback regressions missing"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-library --lib -- --list scan_changes::tests:: 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 2 ] || { say "FAIL: scan content invalidation regressions missing"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-library --lib -- --list unchanged_and_missing_scans_complete_without_catalog_invalidation 2>/dev/null | grep -c ': test$' || true)
[ "$n" -ge 1 ] || { say "FAIL: unchanged scan service regression missing"; exit 1; }

say "gate: signed updater regression suite present"
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-updater --lib -- --list 2>/dev/null | grep -c ': test$' || true)
(( n >= 11 )) || { echo "Updater suite has $n tests (expected >= 11)"; exit 1; }

say "gate: Orbit HTTP host isolation and library profile regressions present"
# The workspace run executes these. Keep actual socket/gate/shutdown tests and
# profile isolation from disappearing during the next extraction phases.
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-control --test http_hosts -- --list 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 3 )) || { echo "Orbit HTTP host suite has $n tests (expected >= 3)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-library --lib -- --list store::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 3 )) || { echo "Library profile suite has $n tests (expected >= 3)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-library --lib -- --list search::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 4 )) || { echo "Library source search suite has $n tests (expected >= 4)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-library --lib -- --list service::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 9 )) || { echo "Library service suite has $n tests (expected >= 9)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-control --test library_hosts -- --list 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 2 )) || { echo "Orbit library HTTP suite has $n tests (expected >= 2)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-control --test library_admin -- --list 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 2 )) || { echo "Orbit library administration suite has $n tests (expected >= 2)"; exit 1; }
if [[ "$(uname -s)" == "Linux" ]]; then
  say "gate: real Orbit daemon instances (isolated roots, no audio or Qobuz session)"
  orbit_target_dir=$(cargo metadata --manifest-path crates/Cargo.toml --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
  python3 scripts/test-orbit-daemon.py --bin "$orbit_target_dir/debug/qbzd"
fi
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-control --lib -- --list logged_in_frame_contains_public_identity_only 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 1 )) || { echo "SSE public-identity regression missing"; exit 1; }

say "gate: SACD physical-sector and scanner regressions present"
# Already executed by the workspace run: keep the container/seek equivalence
# and failed-scan preservation checks from silently disappearing.
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-disc --lib -- --list raw_sector_tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 9 )) || { echo "SACD physical-sector suite has $n tests (expected >= 9)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-library --lib -- --list raw_sacd_scan_ 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 1 )) || { echo "SACD raw scanner suite has $n tests (expected >= 1)"; exit 1; }

say "gate: listen log suite present and green (qbz-app::listen_log)"
# The workspace run above already executes these; this step exists so a
# refactor that silently drops the module's tests (a renamed mod, a cfg gate)
# fails the gate instead of shrinking coverage without a trace. Cheap: the
# test binary is already built.
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-app --lib -- --list listen_log:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 25 )) || { echo "listen_log suite has $n tests (expected >= 25)"; exit 1; }
cargo test --manifest-path crates/Cargo.toml -p qbz-app --lib -- listen_log::

say "gate: account-migration and portable-blacklist suites present and green"
# Same shape as the listen-log check: the workspace run already executed
# these; the count keeps a renamed module or a dropped cfg from shrinking the
# coverage silently. qbz-account-migration is the crate behind Settings >
# Import / Export (snapshot, plan, apply, ledger, local-profile copy);
# blacklist_portable is the JSON one user hands to another.
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-account-migration --lib -- --list 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 13 )) || { echo "qbz-account-migration has $n tests (expected >= 13)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-app --lib -- --list blacklist_portable:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 3 )) || { echo "blacklist_portable suite has $n tests (expected >= 3)"; exit 1; }
cargo test --manifest-path crates/Cargo.toml -p qbz-account-migration --lib
cargo test --manifest-path crates/Cargo.toml -p qbz-app --lib -- blacklist_portable::

say "gate: QConnect controller smoke and consecutive-log regressions present and green"
# Do not silently lose the iOS handoff regression coverage: read queries must
# not feed a resync loop or starve controls, while real queue writes serialize.
# Wire-to-controller tests also cover proto3 item zero, takeback, seek and mute;
# manual skip fixtures cover shuffle/loop/position/autoplay boundaries.
n=$(cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- --list controller_smoke:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 13 )) || { echo "QConnect controller smoke suite has $n tests (expected >= 13)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- --list controller_takeover:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 5 )) || { echo "QConnect takeover suite has $n tests (expected >= 5)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- --list queue_resolution::tests::manual_skip 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 6 )) || { echo "QConnect manual skip suite has $n tests (expected >= 6)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qconnect-protocol --lib -- --list decoder::tests::controller_ 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 3 )) || { echo "QConnect controller wire suite has $n tests (expected >= 3)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-log --lib -- --list repeat::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 8 )) || { echo "consecutive log suite has $n tests (expected >= 8)"; exit 1; }
cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- controller_smoke::
cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- controller_takeover::
cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- queue_resolution::tests::manual_skip
cargo test --manifest-path crates/Cargo.toml -p qconnect-protocol --lib -- decoder::tests::controller_
cargo test --manifest-path crates/Cargo.toml -p qbz-log --lib -- repeat::tests::

say "gate: handoff execution, PCM progress, and bounded CMAF assembly regressions"
n=$(cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- --list handoff_ 2>/dev/null | grep -c ': test$' || true)
(( n >= 8 )) || { echo "handoff suite has $n tests (expected >= 8)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-audio --lib -- --list pcm_write::tests:: 2>/dev/null | grep -c ': test$' || true)
(( n >= 3 )) || { echo "PCM progress suite has $n tests (expected >= 3)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-qobuz --lib -- --list incremental_ 2>/dev/null | grep -c ': test$' || true)
(( n >= 2 )) || { echo "CMAF assembly suite has $n tests (expected >= 2)"; exit 1; }
cargo test --manifest-path crates/Cargo.toml -p qconnect-app --lib -- handoff_
cargo test --manifest-path crates/Cargo.toml -p qbz-audio --lib -- pcm_write::tests::
cargo test --manifest-path crates/Cargo.toml -p qbz-qobuz --lib -- incremental_

say "gate: 2026-09 static-review regressions present (image-cache LRU, loudness-cache degrade, log-rotation lock, URL-free CDN errors + signed-param redaction, link-resolver timeout)"
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-cache --lib -- --list image_cache::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 4 )) || { echo "image cache suite has $n tests (expected >= 4)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-audio --lib -- --list loudness_cache::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 5 )) || { echo "loudness cache suite has $n tests (expected >= 5)"; exit 1; }
# Loudness from the first sample (2026-09): absolute-LUFS cache with ranked
# sources, full-track scan, one-shot live gain, 400 ms glide, start-gain plan.
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-audio --lib -- --list loudness_scan::tests:: 2>/dev/null \
  | grep -c ': test$' || true)
(( n >= 1 )) || { echo "loudness scan suite has $n tests (expected >= 1)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-audio --lib -- --list dynamic_amplify::tests:: 2>/dev/null \
  | grep -c ': test$' || true)
(( n >= 2 )) || { echo "dynamic amplify suite has $n tests (expected >= 2)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-audio --lib -- --list loudness_analyzer::tests:: 2>/dev/null \
  | grep -c ': test$' || true)
(( n >= 2 )) || { echo "loudness analyzer suite has $n tests (expected >= 2)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-player --lib -- --list normalization::tests:: 2>/dev/null \
  | grep -c ': test$' || true)
(( n >= 3 )) || { echo "player normalization suite has $n tests (expected >= 3)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-log --lib -- --list install::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 3 )) || { echo "log rotation suite has $n tests (expected >= 3)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-log --lib -- --list redact::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 7 )) || { echo "redaction suite has $n tests (expected >= 7)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-qobuz --lib -- --list net_diag::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 2 )) || { echo "net_diag suite has $n tests (expected >= 2)"; exit 1; }
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-music-link --lib -- --list fast_path::tests:: 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 1 )) || { echo "link-resolver timeout suite has $n tests (expected >= 1)"; exit 1; }

say "gate: DLNA device-description tolerance present and green (#745)"
# The local rupnp patch accepts legacy service URLs without downgrading http.
# Invalid/oversized URLs must remain rejected.
n=$(cargo test --manifest-path crates/Cargo.toml -p qbz-cast --test dlna_description_tolerance -- --list 2>/dev/null \
    | grep -c ': test$' || true)
(( n >= 4 )) || { echo "DLNA description tolerance suite has $n tests (expected >= 4)"; exit 1; }
cargo test --manifest-path crates/Cargo.toml -p qbz-cast --test dlna_description_tolerance
# Normal v1/v3 renderers must retain the same transport/volume SOAP contract.
cargo test --manifest-path crates/Cargo.toml -p qbz-cast --test dlna_wire_compatibility

say "gate: a fatal signal reports itself (#749 / packaged-AppDir SIGSEGV)"
# The intermittent startup segfault in the shipped AppDir left nothing but
# `status -11`. The reporter must keep naming the signal AND must keep the wait
# status a signal, so CI, the smoke harness and core dumps still see a crash.
cargo test --manifest-path crates/Cargo.toml -p qbz-log --test fatal_signal

say "gate: qbzd resolves no Slint crate"
hits=$(cargo tree --manifest-path crates/Cargo.toml -p qbzd -e normal \
       | grep -E '\b(slint|qbz-ui|qbz-slint-common|qbz-dac-wizard) v' || true)
[[ -z "$hits" ]] || { echo "qbzd graph resolves Slint crates:"; echo "$hits"; exit 1; }

[[ "${QT:-0}" == 1 ]] || { say "done (set QT=1 for the Qt gates)"; exit 0; }

say "qt gate 1/5: QML audits"
for a in qml_resolution_audit qml_singleton_xref qml_eager_tab_audit qml_module_registration_audit qml_icon_bake_audit qml_on_prefixed_property_audit; do
  python3 "scripts/qml-audits/$a.py" "$ROOT/crates/qbz-qt"
done

say "qt gate 2/5: shader bake gate"
scripts/qml-audits/shader_bake_gate.sh "${QSB:-$(command -v qsb || echo /usr/lib64/qt6/bin/qsb)}"

say "qt gate 3/5: qbz-qt resolves no Slint crate"
hits=$(cargo tree --manifest-path crates/Cargo.toml -p qbz-qt -e normal \
       | grep -E '\b(slint|qbz-ui|qbz-slint-common|qbz-dac-wizard) v' || true)
[[ -z "$hits" ]] || { echo "qbz-qt graph resolves Slint crates:"; echo "$hits"; exit 1; }

say "qt gates 4/5 + 5/5: tests and debug/release startup liveness"
bash scripts/qt-runtime-gate.sh
say "done"
