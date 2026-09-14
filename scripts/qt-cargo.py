#!/usr/bin/env python3
"""Run Cargo with the *native* Qt SDK included in cc-rs's cache inputs.

Cargo does not discover changes to system C++ headers. In particular, an
in-place Qt update can leave cxx-qt-lib built with old inline templates while
qbz-qt uses new ones. The linker coalesces their identically named symbols;
this caused the QList<float> startup SIGSEGV on 2026-09-06.

Use this entry point for Qt builds/tests, including direct Cargo invocations:
  python3 scripts/qt-cargo.py build --release --manifest-path crates/Cargo.toml -p qbz-qt

No target cleanup, profile changes, or RUSTFLAGS: a content-derived, unused
C++ define invalidates cc-rs consumers when the SDK changes, in either profile
and with any Rust toolchain. Existing CXXFLAGS (including MSVC flags) survive.

Where Cargo builds follows scripts/qt-target.py: a host-level target shared by
every worktree becomes this worktree's own sibling of it, so a direct build or
test here cannot replace a binary another worktree built. An explicit
CARGO_TARGET_DIR, or a target inside the checkout, is left to Cargo.
"""

import hashlib
import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


def query(qmake, name):
    return subprocess.check_output(
        [qmake, "-query", name], text=True, stderr=subprocess.PIPE, timeout=15
    ).strip()


def find_qmake(env):
    explicit = env.get("QMAKE")
    candidates = [explicit] if explicit else ["qmake6", "qmake"]
    for candidate in candidates:
        executable = shutil.which(candidate, path=env.get("PATH"))
        if executable:
            version = query(executable, "QT_VERSION")
            if re.fullmatch(r"6\.\d+\.\d+(?:[-+].*)?", version):
                # Keep argv[0]: qmake may be a qtchooser symlink.
                return os.path.abspath(executable), version
    raise RuntimeError("Qt 6 qmake not found; set QMAKE to the intended SDK's qmake")


def header_files(root, modules_only=False):
    # Homebrew SDKs can expose headers through directory symlinks. Follow
    # them, but visit each real directory once so a cyclic link cannot hang.
    seen = set()
    for directory, dirs, files in os.walk(root, followlinks=True, onerror=raise_error):
        if modules_only and Path(directory) == root:
            # Homebrew can report /opt/homebrew/include, not a Qt-only root.
            dirs[:] = [name for name in dirs if name.startswith("Qt")]
            files = [name for name in files if name.startswith("Qt")]
        real = Path(directory).resolve()
        if real in seen:
            dirs[:] = []
            continue
        seen.add(real)
        dirs.sort()
        for name in sorted(files):
            yield Path(directory) / name


def raise_error(error):
    raise error


def sdk_fingerprint(qmake, version):
    headers = Path(query(qmake, "QT_INSTALL_HEADERS"))
    libraries = Path(query(qmake, "QT_INSTALL_LIBS"))
    digest = hashlib.sha256()
    # Paths are intentional: switching SDK roots must also rebuild native
    # archives and their recorded linker search paths.
    for value in ("qbz-qt-sdk-v1", qmake, version, str(headers.resolve()),
                  str(libraries)):
        digest.update(value.encode() + b"\0")
    count = 0
    def sdk_headers():
        if headers.is_dir():
            for path in header_files(headers, modules_only=True):
                yield str(path.relative_to(headers)), path
        # cxx-qt-build also consumes these. Homebrew's qmake may point at a
        # shared include root with NO QtCore subdir; its real headers live in
        # QT_INSTALL_LIBS/QtCore.framework/Headers instead.
        for framework in sorted(libraries.glob("Qt*.framework")):
            root = framework / "Headers"
            if root.is_dir():
                for path in header_files(root):
                    yield f"frameworks/{framework.name}/Headers/{path.relative_to(root)}", path

    for name, path in sdk_headers():
        digest.update(name.encode() + b"\0")
        content = path.read_bytes()
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
        count += 1
    if not count:
        raise RuntimeError(f"Qt headers are missing or empty: {headers}, {libraries}")
    return digest.hexdigest()[:32]


def cargo_environment(env):
    result = dict(env)
    qmake, version = find_qmake(result)
    fingerprint = sdk_fingerprint(qmake, version)
    # cc-rs tracks and appends CXXFLAGS even with target-specific flags.
    # Remove only our own previous marker, making nested invocations stable.
    flags = re.sub(r"(?:^|\s)-DQBZ_QT_SDK_[0-9a-f]{32}=1(?=\s|$)", "",
                   result.get("CXXFLAGS", "")).strip()
    result["CXXFLAGS"] = f"{flags} -DQBZ_QT_SDK_{fingerprint}=1".strip()
    result["QMAKE"] = qmake
    return result, version, fingerprint


def load_targets():
    # A hyphenated sibling loaded by path: without this, every build would leave
    # an untracked scripts/__pycache__ in the checkout.
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location(
        "qt_target", Path(__file__).with_name("qt-target.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def manifest_argument(arguments):
    for index, argument in enumerate(arguments):
        if argument == "--manifest-path" and index + 1 < len(arguments):
            return Path(arguments[index + 1])
        if argument.startswith("--manifest-path="):
            return Path(argument.split("=", 1)[1])
    return None


def target_environment(env, arguments, targets=None):
    """`env` with CARGO_TARGET_DIR set to this worktree's own target, when the
    target Cargo would use is shared with other worktrees (qt-target.py)."""
    targets = targets or load_targets()
    if targets.explicit(env):
        return env
    try:
        metadata = targets.cargo_metadata(manifest_argument(arguments), dict(env))
        configured = Path(metadata["target_directory"]).resolve()
        target = targets.worktree_target(
            configured, targets.worktree_root(metadata["workspace_root"]), env)
    except (OSError, subprocess.SubprocessError, KeyError, ValueError) as error:
        print(f"[qt-cargo] target not resolved ({error}); Cargo's own choice stands",
              file=sys.stderr, flush=True)
        return env
    if target == configured:
        return env
    result = dict(env)
    result["CARGO_TARGET_DIR"] = str(target)
    print(f"[qt-cargo] worktree target {target}", file=sys.stderr, flush=True)
    return result


def main():
    if len(sys.argv) < 2:
        raise RuntimeError("usage: qt-cargo.py <cargo subcommand> [arguments...]")
    env, version, fingerprint = cargo_environment(os.environ)
    print(f"[qt-cargo] Qt {version}, SDK {fingerprint}, qmake={env['QMAKE']}",
          file=sys.stderr, flush=True)
    env = target_environment(env, sys.argv[2:])
    # subprocess.run, NOT os.execvpe: on Windows os.exec* is emulated (spawn +
    # terminate) and segfaulted here — the release-windows build died with
    # "Segmentation fault ... exit code 139" right after this line on
    # 2026-09-07, while the POSIX Linux/macOS builds using the SAME wrapper
    # were fine. subprocess is portable and forwards cargo's exit code.
    cargo = shutil.which("cargo", path=env.get("PATH")) or "cargo"
    completed = subprocess.run([cargo, *sys.argv[1:]], env=env)
    sys.exit(completed.returncode)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"[qt-cargo] {error}", file=sys.stderr)
        sys.exit(1)
