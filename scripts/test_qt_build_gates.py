#!/usr/bin/env python3
"""SDK-cache and crash-aware smoke regressions; no Qt or Cargo build needed."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import sys
from unittest.mock import patch

sys.dont_write_bytecode = True


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


qt = load("qt_cargo", "qt-cargo.py")
smoke = load("qt_smoke", "qt-smoke.py")
targets = load("qt_target", "qt-target.py")


@unittest.skipIf(os.name == "nt", "unprivileged Windows symlinks may be unavailable")
class TargetTests(unittest.TestCase):
    """Each worktree keeps its own binaries when the host shares one target."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.worktree = self.root / "checkouts" / "main-tree"
        self.crates = self.worktree / "crates"
        self.crates.mkdir(parents=True)
        self.manifest = self.crates / "Cargo.toml"
        self.manifest.touch()
        self.shared = self.root / "nvme" / "qbz"
        (self.shared / "release" / "deps").mkdir(parents=True)
        (self.shared / "release" / "incremental").mkdir()
        (self.shared / "release" / "deps" / "libdep.rlib").write_bytes(b"registry crate")
        (self.shared / "release" / "incremental" / "session").write_bytes(b"workspace cache")
        (self.shared / "release" / "qbz").write_bytes(b"another worktree's binary")
        self.derived = self.root / "nvme" / "qbz-worktrees" / "main-tree"

    def test_shared_host_target_becomes_this_worktrees_own_sibling(self):
        self.assertEqual(targets.worktree_target(self.shared, self.worktree, {}), self.derived)
        other = self.root / "checkouts" / "codex-tree"
        self.assertEqual(targets.worktree_target(self.shared, other, {}),
                         self.root / "nvme" / "qbz-worktrees" / "codex-tree")

    def test_explicit_override_and_checkout_target_are_used_as_given(self):
        for name in targets.EXPLICIT_ENV:
            with self.subTest(env=name):
                self.assertEqual(targets.worktree_target(self.shared, self.worktree, {name: "/pinned"}),
                                 self.shared)
        inside = self.crates / "target"
        self.assertEqual(targets.worktree_target(inside, self.worktree, {}), inside)

    def test_two_checkouts_with_one_directory_name_never_share_a_target(self):
        self.derived.mkdir(parents=True)
        (self.derived / targets.MARKER).write_text(str(self.root / "elsewhere" / "main-tree") + "\n")
        second = targets.worktree_target(self.shared, self.worktree, {})
        self.assertNotEqual(second, self.derived)
        self.assertTrue(second.name.startswith("main-tree-"))
        self.assertEqual(second, targets.worktree_target(self.shared, self.worktree, {}))
        (self.derived / targets.MARKER).write_text(str(self.worktree) + "\n")
        self.assertEqual(targets.worktree_target(self.shared, self.worktree, {}), self.derived)

    def test_resolve_reads_cargo_metadata_for_the_manifest(self):
        metadata = {"target_directory": str(self.shared), "workspace_root": str(self.crates)}
        with patch.object(targets, "cargo_metadata", return_value=metadata) as cargo, \
                patch.object(targets, "worktree_root", return_value=self.worktree):
            self.assertEqual(targets.resolve_target(self.manifest, {}), self.derived)
        self.assertEqual(cargo.call_args.args[0], self.manifest)

    def test_link_keeps_the_familiar_path_and_claims_the_worktree_target(self):
        (self.derived / "release").mkdir(parents=True)
        (self.derived / "release" / "qbz").write_bytes(b"this worktree's binary")
        targets.link_target(self.manifest, self.derived, self.worktree)
        targets.link_target(self.manifest, self.derived, self.worktree)
        alias = self.crates / "target"
        self.assertTrue(alias.is_symlink())
        self.assertEqual((alias / "release" / "qbz").read_bytes(), b"this worktree's binary")
        self.assertEqual((self.derived / targets.MARKER).read_text().strip(), str(self.worktree))
        # A pinned target is linked but never claimed.
        targets.link_target(self.manifest, self.shared, self.worktree)
        self.assertFalse((self.shared / targets.MARKER).exists())

    def test_real_directory_is_never_removed(self):
        alias = self.crates / "target"
        alias.mkdir()
        sentinel = alias / "keep"
        sentinel.write_text("preserve")
        targets.link_target(self.manifest, alias)
        with self.assertRaisesRegex(RuntimeError, "preserving"):
            targets.link_target(self.manifest, self.shared)
        self.assertEqual(sentinel.read_text(), "preserve")
        self.assertFalse(alias.is_symlink())

    def test_old_shortcut_is_replaced_but_a_missing_build_cannot_replace_it(self):
        alias = self.crates / "target"
        alias.symlink_to(self.root / "old", target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, "does not exist"):
            targets.link_target(self.manifest, self.root / "missing")
        self.assertEqual(os.readlink(alias), str(self.root / "old"))
        targets.link_target(self.manifest, self.shared)
        self.assertEqual(alias.resolve(), self.shared)

    @staticmethod
    def copy(entries, destination):
        for entry in entries:
            if entry.is_dir():
                shutil.copytree(entry, destination / entry.name)
            else:
                shutil.copy2(entry, destination / entry.name)

    def test_seed_clones_a_missing_profile_once_and_skips_incremental(self):
        self.assertEqual(targets.seed_target(self.derived, self.shared, ["release", "debug"], self.copy),
                         ["release"])
        self.assertEqual((self.derived / "release" / "deps" / "libdep.rlib").read_bytes(), b"registry crate")
        self.assertFalse((self.derived / "release" / "incremental").exists())
        (self.derived / "release" / "qbz").write_bytes(b"this worktree's binary")
        self.assertEqual(targets.seed_target(self.derived, self.shared, ["release"], self.copy), [])
        self.assertEqual((self.derived / "release" / "qbz").read_bytes(), b"this worktree's binary")
        self.assertEqual((self.shared / "release" / "qbz").read_bytes(), b"another worktree's binary")

    def test_seed_never_touches_pinned_targets_and_leaves_no_partial_clone(self):
        pinned = self.root / "pinned"
        self.assertEqual(targets.seed_target(pinned, self.shared, ["release"], self.copy), [])
        self.assertFalse(pinned.exists())
        self.assertEqual(targets.seed_target(self.shared, self.shared, ["release"], self.copy), [])

        def broken(entries, destination):
            (destination / "half").write_bytes(b"partial")
            raise OSError("clone not supported")

        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(targets.seed_target(self.derived, self.shared, ["release"], broken), [])
        self.assertEqual(list(self.derived.iterdir()), [])


class CargoTargetEnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.worktree = self.root / "tree"
        self.shared = self.root / "shared" / "qbz"
        self.metadata = {"target_directory": str(self.shared), "workspace_root": str(self.worktree / "crates")}
        for name, value in (("cargo_metadata", lambda *_: self.metadata),
                            ("worktree_root", lambda *_: self.worktree)):
            mock = patch.object(targets, name, side_effect=value)
            mock.start()
            self.addCleanup(mock.stop)

    def environment(self, env, arguments=("--manifest-path", "crates/Cargo.toml")):
        with contextlib.redirect_stderr(io.StringIO()):
            return qt.target_environment(env, list(arguments), targets)

    def test_direct_builds_and_tests_use_the_worktree_target(self):
        env = self.environment({"PATH": "/bin"})
        self.assertEqual(env["CARGO_TARGET_DIR"], str(self.root / "shared" / "qbz-worktrees" / "tree"))
        self.assertEqual(targets.cargo_metadata.call_args.args[0], Path("crates/Cargo.toml"))

    def test_explicit_and_checkout_targets_are_left_to_cargo(self):
        pinned = {"CARGO_TARGET_DIR": "/pinned"}
        self.assertIs(self.environment(pinned), pinned)
        self.metadata["target_directory"] = str(self.worktree / "crates" / "target")
        plain = {"PATH": "/bin"}
        self.assertIs(self.environment(plain), plain)

    def test_unresolvable_metadata_keeps_cargos_own_choice(self):
        targets.cargo_metadata.side_effect = subprocess.CalledProcessError(101, "cargo")
        plain = {"PATH": "/bin"}
        self.assertIs(self.environment(plain), plain)

    def test_manifest_argument_forms(self):
        self.assertEqual(qt.manifest_argument(["-p", "qbz-qt", "--manifest-path", "a/Cargo.toml"]),
                         Path("a/Cargo.toml"))
        self.assertEqual(qt.manifest_argument(["--manifest-path=b/Cargo.toml"]), Path("b/Cargo.toml"))
        self.assertIsNone(qt.manifest_argument(["--release"]))


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "requires bash")
class RunnerTargetTests(unittest.TestCase):
    def test_runner_builds_in_the_worktree_target_links_after_success_and_runs_it(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            root = base / "main tree"
            scripts = root / "scripts"
            scripts.mkdir(parents=True)
            (root / "crates").mkdir()
            (root / "crates/Cargo.toml").touch()
            shared = base / "shared target"
            expected = base / "shared target-worktrees" / "main-tree"
            for filename in ("qt-run.sh", "qt-target.py"):
                shutil.copyfile(Path(__file__).with_name(filename), scripts / filename)
            fake_bin = base / "bin"
            fake_bin.mkdir()
            cargo = fake_bin / "cargo"
            cargo.write_text('#!/bin/sh\n[ "$FAIL_METADATA" = 1 ] && exit 7\nprintf \'%s\\n\' "$TARGET_METADATA"\n')
            cargo.chmod(0o755)
            git = fake_bin / "git"
            git.write_text("#!/bin/sh\nexit 128\n")
            git.chmod(0o755)
            (scripts / "qt-cargo.py").write_text(
                'import os, pathlib, sys\n'
                'target = pathlib.Path(os.environ["CARGO_TARGET_DIR"])\n'
                'assert target == pathlib.Path(os.environ["EXPECTED_TARGET"]), target\n'
                'if os.environ.get("FAIL_BUILD") == "1": sys.exit(9)\n'
                '(target / "release").mkdir(parents=True, exist_ok=True)\n'
                'binary = target / "release/qbz"\n'
                'binary.write_text("#!/bin/sh\\nprintf \'artifact executed: %s\\\\n\' \\"$1\\"\\n")\n'
                'binary.chmod(0o755)\n'
            )
            metadata = json.dumps({"target_directory": str(shared), "workspace_root": str(root / "crates")})
            env = dict(os.environ, PATH=str(fake_bin) + os.pathsep + os.environ["PATH"],
                       TARGET_METADATA=metadata, EXPECTED_TARGET=str(expected), NO_AUDIT="1",
                       NO_TICKER="1", FORCE="1", NORUN="0", SMOKE="0", TEST="0", DEBUG="0",
                       MOLD="0", XDG_CACHE_HOME=str(base / "cache"), FAIL_METADATA="1", FAIL_BUILD="0")
            for name in targets.EXPLICIT_ENV:
                env.pop(name, None)
            command = ["bash", str(scripts / "qt-run.sh"), "fixture-argument"]
            failed = subprocess.run(command, env=env, text=True, capture_output=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertFalse(expected.exists())
            env.update(FAIL_METADATA="0", FAIL_BUILD="1")
            failed = subprocess.run(command, env=env, text=True, capture_output=True)
            self.assertEqual(failed.returncode, 9, failed.stderr)
            self.assertFalse((root / "crates/target").is_symlink())
            env["FAIL_BUILD"] = "0"
            result = subprocess.run(command, env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("artifact executed: fixture-argument", result.stdout)
            self.assertEqual((root / "crates/target/release/qbz").resolve(), expected / "release/qbz")
            self.assertFalse((shared / "release/qbz").exists(), "the shared target is never built into")


class SdkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.headers = self.root / "include" / "QtCore"
        self.headers.mkdir(parents=True)
        self.header = self.headers / "qarraydataops.h"
        self.header.write_text("old-layout")
        self.answers = {"QT_VERSION": "6.11.2", "QT_INSTALL_HEADERS": str(self.headers.parent),
                        "QT_INSTALL_LIBS": str(self.root / "lib")}
        mock = patch.object(qt, "query", side_effect=lambda _, key: self.answers[key])
        mock.start()
        self.addCleanup(mock.stop)

    def fingerprint(self):
        return qt.sdk_fingerprint("/sdk/bin/qmake", self.answers["QT_VERSION"])

    def test_identical_headers_are_stable_after_touch(self):
        before = self.fingerprint()
        os.utime(self.header, None)
        self.assertEqual(before, self.fingerprint())

    def test_header_change_invalidates_even_with_same_version_size_and_mtime(self):
        before = self.fingerprint()
        stat = self.header.stat()
        self.header.write_text("new-layout")
        os.utime(self.header, ns=(stat.st_atime_ns, stat.st_mtime_ns))
        self.assertNotEqual(before, self.fingerprint())

    def test_patch_version_invalidates(self):
        before = self.fingerprint()
        self.answers["QT_VERSION"] = "6.11.3"
        self.assertNotEqual(before, self.fingerprint())

    def test_library_root_invalidates(self):
        before = self.fingerprint()
        self.answers["QT_INSTALL_LIBS"] = "/other-sdk/lib"
        self.assertNotEqual(before, self.fingerprint())

    def test_added_and_removed_headers_invalidate(self):
        before = self.fingerprint()
        extra = self.headers / "qnew.h"
        extra.write_text("new type")
        self.assertNotEqual(before, self.fingerprint())
        extra.unlink()
        self.assertEqual(before, self.fingerprint())

    @unittest.skipIf(os.name == "nt", "unprivileged Windows symlinks may be unavailable")
    def test_framework_symlink_and_cycle_are_bounded(self):
        framework = self.root / "framework"
        framework.mkdir()
        header = framework / "qframework.h"
        header.write_text("old")
        (self.headers / "framework").symlink_to(framework, target_is_directory=True)
        (framework / "cycle").symlink_to(self.headers, target_is_directory=True)
        before = self.fingerprint()
        header.write_text("new")
        self.assertNotEqual(before, self.fingerprint())

    def test_missing_sdk_fails_closed(self):
        self.answers["QT_INSTALL_HEADERS"] = str(self.root / "missing")
        with self.assertRaisesRegex(RuntimeError, "missing"):
            self.fingerprint()

    def test_empty_sdk_fails_closed(self):
        self.header.unlink()
        with self.assertRaisesRegex(RuntimeError, "empty"):
            self.fingerprint()

    def test_homebrew_framework_only_headers_are_hashed(self):
        self.header.unlink()
        headers = self.root / "lib" / "QtCore.framework" / "Headers"
        headers.mkdir(parents=True)
        header = headers / "qarraydataops.h"
        header.write_text("old")
        before = self.fingerprint()
        header.write_text("new")
        self.assertNotEqual(before, self.fingerprint())

    def test_shared_include_root_ignores_non_qt_packages(self):
        before = self.fingerprint()
        unrelated = self.headers.parent / "other-package"
        unrelated.mkdir()
        (unrelated / "other.h").write_text("not part of Qt")
        self.assertEqual(before, self.fingerprint())

    def test_existing_flags_survive_and_nested_calls_are_stable(self):
        original = {"CXXFLAGS": "/Zc:__cplusplus /permissive- -include arm_acle.h",
                    "RUSTFLAGS": "-C link-arg=-fuse-ld=lld", "CARGO_TARGET_DIR": "/cache"}
        with patch.object(qt, "find_qmake", return_value=("/sdk/bin/qmake", "6.11.2")):
            env, _, _ = qt.cargo_environment(original)
            again, _, _ = qt.cargo_environment(env)
        self.assertEqual(env, again)
        self.assertTrue(env["CXXFLAGS"].startswith(original["CXXFLAGS"] + " "))
        self.assertEqual(env["RUSTFLAGS"], original["RUSTFLAGS"])
        self.assertEqual(env["CARGO_TARGET_DIR"], "/cache")
        self.assertNotIn("QMAKE", original)

    def test_broken_explicit_qmake_does_not_fall_back(self):
        with patch.object(qt.shutil, "which", return_value=None) as which:
            with self.assertRaises(RuntimeError):
                qt.find_qmake({"QMAKE": "/missing/sdk/qmake"})
        which.assert_called_once_with("/missing/sdk/qmake", path=None)


class SmokeTests(unittest.TestCase):
    output = "QbzCore initialized\n" + "normal startup log\n" * 10

    def test_success_requires_liveness(self):
        smoke.check_result(self.output, True, -15)

    def test_early_exit_is_never_green_even_after_success_marker(self):
        for status in (0, 1, 139, -11, -6):
            with self.subTest(status=status), self.assertRaisesRegex(RuntimeError, "exited"):
                smoke.check_result(self.output, False, status)

    def test_crash_at_deadline_is_not_our_intentional_shutdown(self):
        for status in (139, -11, 134, -6, 1):
            with self.subTest(status=status), self.assertRaisesRegex(RuntimeError, "status"):
                smoke.check_result(self.output, True, status)

    def test_silent_hang_is_not_startup(self):
        with self.assertRaises(RuntimeError):
            smoke.check_result("", True, -15)

    def test_missing_core_marker_fails(self):
        with self.assertRaisesRegex(RuntimeError, "initialization"):
            smoke.check_result("log\n" * 12, True, -15)

    def test_qml_error_after_init_fails(self):
        with self.assertRaisesRegex(RuntimeError, "QML complaints"):
            smoke.check_result(self.output + "ReferenceError: missing binding", True, -15)

    def test_known_property_cache_warning_does_not_hide_real_error(self):
        warning = "qt.qml.propertyCache: has no property\n"
        smoke.check_result(self.output + warning, True, -15)
        with self.assertRaises(RuntimeError):
            smoke.check_result(self.output + warning + "TypeError: broken", True, -15)


if __name__ == "__main__":
    unittest.main()
