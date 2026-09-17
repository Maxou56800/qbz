#!/usr/bin/env python3
"""Orbit against real qbzd processes, isolated profiles and no audio/session bus.

Uses a prebuilt binary; never compiles, authenticates to Qobuz or plays audio.
Linux XDG fixture only. Run with --bin /path/to/qbzd after the Rust test build.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import wave


def check(condition, message):
    if not condition:
        raise AssertionError(message)


class Host:
    def __init__(self, base, name, binary):
        self.root = base / name
        self.binary = binary
        self.token = name + "-fixture-key"
        self.env = os.environ.copy()
        for key, folder in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                            ("XDG_CACHE_HOME", "cache"), ("XDG_RUNTIME_DIR", "run")]:
            path = self.root / folder
            path.mkdir(parents=True, mode=0o700)
            self.env[key] = str(path)
        self.env.update({
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=" + str(self.root / "absent-bus"),
            "PIPEWIRE_RUNTIME_DIR": str(self.root / "run"),
            "PIPEWIRE_REMOTE": "orbit-fixture-absent",
            "PULSE_SERVER": "unix:" + str(self.root / "absent-pulse"),
            "QBZD_MPRIS": "0", "QBZD_TOKEN": self.token,
            "PATH": str(base / "fake-tools") + os.pathsep + self.env.get("PATH", ""),
        })
        self.env.pop("QBZD_HOST", None)
        self.env.pop("QBZD_HOOK", None)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        config = self.root / "config/qbzd"
        config.mkdir()
        (config / "qbzd.toml").write_text(
            f'[server]\nbind = "127.0.0.1"\nport = {self.port}\ntoken = "{self.token}"\n')
        self.music = self.root / "music"
        self.music.mkdir()
        with wave.open(str(self.music / f"Orbit {name}.wav"), "wb") as audio:
            audio.setnchannels(1)
            audio.setsampwidth(2)
            audio.setframerate(44100)
            audio.writeframes(b"\0\0" * 44100)
        self.process = None
        self.log = None

    def start(self, orbit):
        self.log = (self.root / "daemon.log").open("ab")
        self.process = subprocess.Popen([self.binary, "run"] + (["--orbit"] if orbit else []),
                                        env=self.env, stdout=self.log, stderr=subprocess.STDOUT)
        end = time.monotonic() + 20
        while time.monotonic() < end:
            check(self.process.poll() is None, f"daemon exited: {self.root / 'daemon.log'}")
            try:
                if self.http("/api/ping")[0] == 200:
                    # The API starts before run() installs its signal waiter.
                    # Do not SIGTERM a booting process in that small window.
                    status = Path(f"/proc/{self.process.pid}/status").read_text()
                    caught = next(line.split()[1] for line in status.splitlines() if line.startswith("SigCgt:"))
                    if int(caught, 16) & (1 << (signal.SIGTERM - 1)):
                        return
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.05)
        raise AssertionError("daemon startup timed out")

    def stop(self):
        if self.process is not None:
            self.process.send_signal(signal.SIGTERM)
            try:
                code = self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
                raise AssertionError("daemon failed to shut down")
            finally:
                self.process = None
                self.log.close()
            check(code == 0, f"unclean shutdown: {code}")

    def http(self, path, body=None, token=None):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}",
                                     data=None if body is None else json.dumps(body).encode(),
                                     headers={"Authorization": "Bearer " + (self.token if token is None else token),
                                              "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=5) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    def cli(self, *args):
        result = subprocess.run([self.binary, "--host", f"127.0.0.1:{self.port}", "library", *args],
                                env=self.env, capture_output=True, text=True, timeout=15)
        check(result.returncode == 0, result.stderr)
        return json.loads(result.stdout)

    def indexed(self):
        end = time.monotonic() + 15
        while time.monotonic() < end:
            state = self.cli("jobs")["library"]
            if not state["progress"]["running"]:
                check(state["last_scan"]["outcome"] == "complete", state)
                return
            time.sleep(0.05)
        raise AssertionError("scan did not complete")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", required=True)
    args = parser.parse_args()
    binary = str(Path(args.bin).resolve(strict=True))
    if not os.sys.platform.startswith("linux"):
        raise SystemExit("this fixture requires Linux XDG isolation")
    with tempfile.TemporaryDirectory(prefix="orbit-daemon-") as scratch:
        base = Path(scratch)
        fake = base / "fake-tools"
        fake.mkdir()
        # Daemon quit's existing clock-reset subprocesses must not reach the
        # user's PipeWire session, even if its environment changes in future.
        for name in ("pw-metadata", "pw-cli"):
            executable = fake / name
            executable.write_text("#!/bin/sh\nexit 0\n")
            executable.chmod(0o700)
        a, b = Host(base, "Alpha", binary), Host(base, "Beta", binary)
        try:
            a.start(False)
            check(a.http("/api/orbit/library")[0] == 404, "Orbit visible without flag")
            check(not (a.root / "data/qbzd/library.db").exists(), "disabled Orbit created a DB")
            a.stop()
            a.start(True)
            b.start(True)
            for host in (a, b):
                code, state = host.http("/api/status")
                check(code == 200 and state["auth"]["state"] == "needs_auth", state)
                check(host.cli("info")["tracks"] == 0, "nonempty new library")
                folders = host.cli("add", str(host.music))
                check(len(folders["folders"]) == 1, folders)
                host.cli("scan")
                host.indexed()
                check(host.cli("info")["tracks"] == 1, "scan missing indexed WAV")
            ia, ib = a.cli("info"), b.cli("info")
            check(ia["instance"] != ib["instance"], "shared host instance")
            pa, pb = a.cli("search", "Orbit"), b.cli("search", "Orbit")
            check(pa["tracks"][0]["id"] == pb["tracks"][0]["id"], "fixture needs colliding native IDs")
            check("Alpha" in pa["tracks"][0]["title"] and "Beta" in pb["tracks"][0]["title"], "crossed host metadata")
            check(a.http("/api/orbit/library/scan", {"instance":ib["instance"]})[0] == 409, "crossed instance admitted")
            check(a.http("/api/orbit/library", token=b.token)[0] == 401, "crossed token admitted")
            a.stop()
            a.start(True)
            check(a.cli("info")["tracks"] == 1, "library did not survive restart")
            check(a.http("/api/orbit/library/scan", {"instance":ia["instance"]})[0] == 409, "old instance admitted after restart")
            check(b.cli("info")["instance"] == ib["instance"], "other host changed during restart")
            print("PASS: flag gate, two daemon roots, no Qobuz auth, folder/scan/search CLI, access/instance isolation, restart persistence")
        except Exception:
            for host in (a, b):
                log = host.root / "daemon.log"
                if log.exists():
                    print(log.read_text(errors="replace")[-6000:], file=os.sys.stderr)
            raise
        finally:
            try:
                a.stop()
            finally:
                b.stop()


if __name__ == "__main__":
    main()
