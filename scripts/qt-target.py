#!/usr/bin/env python3
"""Each worktree's own physical Cargo target, and its crates/target shortcut.

A host may put Cargo's target OUTSIDE the checkout, on a build disk shared by
every worktree below one project directory. Then every worktree compiled into
the SAME directory, and a build in any of them replaced the `qbz` binary another
worktree had just built and was running or smoke-testing (2026-09-14: a parallel
session's release build replaced the binary under test in the main worktree).

So when the target Cargo resolves lies outside the worktree, and nobody pinned it
explicitly with CARGO_TARGET_DIR or CARGO_BUILD_TARGET_DIR, the worktree builds
in its own sibling directory instead:

    <configured target>-worktrees/<worktree directory name>

Building the same worktree again still replaces its own binary; other worktrees
keep theirs. A target inside the checkout (Cargo's default: macOS, CI, Windows)
and an explicit override are returned unchanged. No machine-specific path lives
here: Cargo configuration and the environment choose the storage.

The first build of a profile in a new worktree target is seeded from the shared
target with copy-on-write clones (`cp --reflink=always`), so the registry crates
that did not change are not compiled again and no data is duplicated; where the
filesystem cannot clone, nothing is copied and Cargo simply builds from scratch.
Never remove a real target directory to make room for a symlink.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import uuid

EXPLICIT_ENV = ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")
MARKER = ".qbz-worktree"
SUFFIX = "-worktrees"
# Workspace crates recompile per worktree anyway (their metadata hash includes
# the checkout path), and incremental caches are only kept for those.
SEED_SKIP = {"incremental"}


def cargo_metadata(manifest=None, env=None):
    command = ["cargo", "metadata", "--no-deps", "--format-version", "1"]
    if manifest is not None:
        command += ["--manifest-path", str(manifest)]
    return json.loads(subprocess.check_output(command, text=True, env=env))


def worktree_root(workspace_root):
    workspace_root = Path(workspace_root)
    try:
        top = subprocess.check_output(
            ["git", "-C", str(workspace_root), "rev-parse", "--show-toplevel"],
            text=True, stderr=subprocess.DEVNULL).strip()
        if top:
            return Path(top).resolve()
    except (OSError, subprocess.SubprocessError):
        pass
    # This repository keeps its Cargo workspace in crates/.
    return workspace_root.resolve().parent


def is_inside(path, root):
    try:
        Path(path).relative_to(root)
        return True
    except ValueError:
        return False


def explicit(env):
    return any(env.get(name) for name in EXPLICIT_ENV)


def worktree_target(configured, root, env):
    """The target this worktree builds in (see the module docstring)."""
    configured = Path(configured).resolve()
    root = Path(root).resolve()
    if explicit(env) or is_inside(configured, root):
        return configured
    name = re.sub(r"[^A-Za-z0-9._-]+", "-", root.name).strip("-") or "worktree"
    candidate = configured.parent / f"{configured.name}{SUFFIX}" / name
    try:
        claimed = (candidate / MARKER).read_text().strip()
    except OSError:
        claimed = ""
    if claimed and claimed != str(root):
        # Two checkouts with the same directory name: the second one gets a
        # stable suffix instead of sharing (and overwriting) the first.
        digest = hashlib.sha1(str(root).encode()).hexdigest()[:8]
        candidate = candidate.with_name(f"{name}-{digest}")
    return candidate


def shared_target(manifest=None, env=None):
    """Cargo's configured target with any explicit override removed."""
    env = dict(os.environ if env is None else env)
    for name in EXPLICIT_ENV:
        env.pop(name, None)
    return Path(cargo_metadata(manifest, env)["target_directory"]).resolve()


def resolve_target(manifest=None, env=None):
    env = os.environ if env is None else env
    metadata = cargo_metadata(manifest, dict(env))
    root = worktree_root(metadata["workspace_root"])
    return worktree_target(metadata["target_directory"], root, env)


def is_worktree_target(target, shared):
    target = Path(target).resolve()
    shared = Path(shared).resolve()
    return target.parent == shared.parent / f"{shared.name}{SUFFIX}"


def link_target(manifest, target, root=None):
    target = Path(target).resolve()
    alias = Path(manifest).resolve().parent / "target"
    if not target.is_dir():
        raise RuntimeError(f"build target does not exist: {target}")
    if root is not None and target.parent.name.endswith(SUFFIX):
        # Claim this worktree target for its checkout (see worktree_target).
        (target / MARKER).write_text(f"{Path(root).resolve()}\n")
    if alias.resolve() == target:
        return
    if alias.exists() and not alias.is_symlink():
        raise RuntimeError(f"preserving existing target directory: {alias}; migrate it before linking")
    if os.name == "nt":
        # A normal Windows Cargo target already uses alias. External targets
        # must remain usable without requiring symlink privileges in CI.
        print(f"[qt-target] external target: {target} (shortcut not created on Windows)", file=sys.stderr)
        return
    temporary = alias.with_name(f".target-link-{uuid.uuid4().hex}")
    try:
        temporary.symlink_to(target, target_is_directory=True)
        temporary.replace(alias)
    finally:
        temporary.unlink(missing_ok=True)


def seed_target(target, shared, profiles, clone=None):
    """Clone each missing profile directory of `shared` into `target`.

    Returns the profiles seeded. A profile already present in `target`, absent
    from `shared`, or that the filesystem cannot clone is left alone.
    """
    target = Path(target)
    shared = Path(shared)
    clone = clone or reflink_clone
    seeded = []
    if target.resolve() == shared.resolve() or not is_worktree_target(target, shared):
        return seeded
    for profile in profiles:
        source = shared / profile
        destination = target / profile
        if destination.exists() or not source.is_dir():
            continue
        target.mkdir(parents=True, exist_ok=True)
        staging = target / f".seed-{profile}-{uuid.uuid4().hex}"
        staging.mkdir()
        try:
            entries = sorted(entry for entry in source.iterdir() if entry.name not in SEED_SKIP)
            if entries:
                clone(entries, staging)
            # Appears whole or not at all: an interrupted clone never looks
            # like a finished target to Cargo.
            staging.rename(destination)
            seeded.append(profile)
        except (OSError, subprocess.SubprocessError) as error:
            print(f"[qt-target] not seeding {profile} ({error}); Cargo builds it from scratch", file=sys.stderr)
        finally:
            if staging.exists():
                shutil.rmtree(staging, ignore_errors=True)
    return seeded


def reflink_clone(entries, destination):
    if not sys.platform.startswith("linux"):
        raise OSError("copy-on-write clones are only used with GNU cp")
    subprocess.run(["cp", "-a", "--reflink=always", *map(str, entries), str(destination)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("operation", choices=["resolve", "link", "seed"])
    parser.add_argument("profiles", nargs="*", help="seed: profile directories, e.g. release debug")
    parser.add_argument("--manifest-path", default="crates/Cargo.toml", type=Path)
    parser.add_argument("--target-dir", type=Path)
    args = parser.parse_args()
    if args.operation == "resolve":
        print(args.target_dir.resolve() if args.target_dir else resolve_target(args.manifest_path))
        return
    target = args.target_dir.resolve() if args.target_dir else resolve_target(args.manifest_path)
    if args.operation == "link":
        root = worktree_root(Path(args.manifest_path).resolve().parent)
        link_target(args.manifest_path, target, root)
        return
    shared = shared_target(args.manifest_path)
    for profile in seed_target(target, shared, args.profiles or ["release"]):
        print(f"[qt-target] seeded {profile} from {shared / profile}", file=sys.stderr)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError, KeyError, ValueError) as error:
        print(f"[qt-target] {error}", file=sys.stderr)
        sys.exit(1)
