# qbzd — QBZ headless Qobuz playback daemon (standalone download)

This tarball is the independent daemon download (no dependency on the desktop
`qbz` app, no deb/rpm needed). Install it on the box wired to your DAC — a
Raspberry Pi, an LXC, a living-room NUC — run `qbzd setup` once, and it
appears in the official Qobuz app as a Qobuz Connect device.

## Contents

- `qbzd` — the daemon binary (also its own CLI client and setup TUI)
- `qbzd.service` — a systemd user unit (shipped, not enabled); the binary can
  also generate systemd system units, OpenRC scripts, and runit services
- `completions/` — bash/zsh/fish shell completions

## Install

Recommended — matches the shipped unit's `ExecStart=/usr/bin/qbzd run`:

```bash
sudo install -Dm755 qbzd /usr/bin/qbzd
sudo install -Dm644 qbzd.service /usr/lib/systemd/user/qbzd.service
systemctl --user daemon-reload
```

Prefer a user-local install instead? Copy `qbzd` anywhere on your `$PATH`
(e.g. `~/.local/bin/qbzd`), then edit `qbzd.service`'s `ExecStart=` line to
point at that path before copying it to `~/.config/systemd/user/qbzd.service`
and running `systemctl --user daemon-reload`.

Shell completions (optional):

```bash
sudo cp completions/qbzd.bash /usr/share/bash-completion/completions/qbzd
# zsh: copy completions/qbzd.zsh into a directory on your $fpath
# fish: copy completions/qbzd.fish into ~/.config/fish/completions/
```

## Required: enable linger

Without linger, the user unit stops the moment you log out of SSH and the
device vanishes from the Qobuz app:

```bash
sudo loginctl enable-linger $USER
```

`qbzd status` warns when linger is off.

## First run

```bash
qbzd setup
```

`qbzd setup` is the six-screen configurator: log in to Qobuz, pick the audio
device, name the Connect device. It edits the same stores `qbzd run` reads,
so one pass is enough (revisit any time to change a setting).

Then enable and start the daemon:

```bash
systemctl --user enable --now qbzd
systemctl --user status qbzd
```

## Playback memory cache

The recommended fixed default remains 400 MiB (50 MiB on a low-memory host).
For long Hi-Res tracks, optional growth uses available memory up to your ceiling:

```bash
qbzd settings set audio.playback_cache_max_mib 1600
qbzd settings set audio.playback_cache_min_mib 400
qbzd settings set audio.playback_cache_dynamic true
qbzd status --json
```

`playback_cache` in the status JSON reports usage and effective budgets. Limits
are MiB; `none` clears an override. Restore defaults with `dynamic false` and
both limits `none`. The base is not reserved RAM; pressure can reduce it. These
budgets exclude active streaming buffers, decoders and offline downloads. The
policy applies live without reopening the output device.


## Network access

By default `qbzd run` listens on `0.0.0.0:8182`: any device on your network
can control playback, the queue and your playlists, the same way a Sonos or
Chromecast renderer is open on the LAN. Browsers are blocked by an Origin
check; other programs are not. The daemon logs one line about this at boot.

To restrict it, edit `~/.config/qbzd/qbzd.toml`:

```toml
[server]
bind = "127.0.0.1"   # local clients only
# or keep the LAN bind and require a token on every request:
token = "choose-a-long-random-string"
```

With a token set, clients send `Authorization: Bearer <token>`; the `qbzd`
CLI on the same machine picks it up from `qbzd.toml` automatically (or from
`QBZD_TOKEN`). `qbzd setup` has the same field on its Network screen
("Access token").

## OpenRC and runit

Do not reuse the systemd unit on a non-systemd host. `qbzd` generates a
service definition for the init system that is actually running and fills in
the target user's `HOME`, group, UID-derived runtime directory, and binary
path. Run it as the playback user so auto-detection and defaults describe the
right account:

```bash
qbzd service --user "$USER" --bin /usr/bin/qbzd
```

The generated definition is written to stdout and the exact install/enable
commands are written to stderr. You can also select the init explicitly:

```bash
# OpenRC
qbzd service openrc --user "$USER" --bin /usr/bin/qbzd \
  | sudo tee /etc/init.d/qbzd >/dev/null
sudo chmod +x /etc/init.d/qbzd
sudo rc-update add qbzd default

# runit (adjust the enabled-service symlink for the distribution)
sudo mkdir -p /etc/sv/qbzd
qbzd service runit --user "$USER" --bin /usr/bin/qbzd \
  | sudo tee /etc/sv/qbzd/run >/dev/null
sudo chmod +x /etc/sv/qbzd/run
```

The OpenRC/runit service is system-scoped but drops privileges to the selected
user. Keep that user in the `audio` group for ALSA Direct and ensure its
`/run/user/<uid>` exists when using PipeWire or PulseAudio.

## System service (appliances and HiFiBerryOS)

`qbzd` can also emit a **system-scoped** unit. This is the form an appliance
manager can control with ordinary `systemctl` even when no user is logged in:

```bash
qbzd service systemd --system --user "$USER" --bin /usr/bin/qbzd \
  | sudo tee /etc/systemd/system/qbzd.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now qbzd
sudo systemctl status qbzd
```

`--system` changes who owns and manages the unit; it deliberately does **not**
run the audio player as root. The generated unit contains `User=`, `HOME=` and
`XDG_RUNTIME_DIR=` for the selected account. Keep that account in the `audio`
group when using ALSA Direct, and enable linger when its PipeWire/Pulse socket
lives under `/run/user/<uid>`:

```bash
sudo usermod -aG audio "$USER"
sudo loginctl enable-linger "$USER"
```

HiFiBerryOS Next Generation can register an external player through drop-in
files. Point its `systemd_service` field at `qbzd`; the upstream
[Add your own player](https://github.com/hifiberry/hifiberry-os/blob/main/docs/add-your-own-player.md)
guide documents the player descriptor, AudioControl registration and
config-server permission files. A native HiFiBerry package can therefore own
the unit and expose its on/off switch without making `qbzd` a root process.

## Read-only root filesystems

All default profile paths follow the standard XDG variables. Redirect them to
writable mounts before running `qbzd setup`, then use the same environment in
the service. This example keeps configuration and databases on persistent
storage, including the playback cache. Use a disk-backed writable mount on
small-memory appliances; a tmpfs cache consumes RAM even when playback uses
file readers (replace `/data` and the `qbz` account with appliance paths/users):

```bash
sudo install -d -o qbz -g qbz /data/qbzd-config /data/qbzd-data /data/qbzd-cache
sudo -u qbz env \
  XDG_CONFIG_HOME=/data/qbzd-config \
  XDG_DATA_HOME=/data/qbzd-data \
  XDG_CACHE_HOME=/data/qbzd-cache \
  /usr/bin/qbzd setup
```

Add the same paths to the system unit with `sudo systemctl edit qbzd`:

```ini
[Service]
Environment=XDG_CONFIG_HOME=/data/qbzd-config
Environment=XDG_DATA_HOME=/data/qbzd-data
Environment=XDG_CACHE_HOME=/data/qbzd-cache
```

After `sudo systemctl daemon-reload && sudo systemctl restart qbzd`, the roots
reported by `qbzd status` should all be writable. As a simpler container-style
alternative, set `data_root = "/writable/qbzd-state"` in `qbzd.toml`; this
moves both application data and its cache below that directory. The XDG config
root still holds `qbzd.toml` and the login credential, so it must remain
readable (and writable whenever login/settings are changed).

## Playback memory on Raspberry Pi

Choose the same profiles offered by the Qt app with
`qbzd settings set audio.playback_memory_profile <auto|high|desktop|low|custom>`.
Auto is the recommended default. High enables dynamic growth from 400 to
1600 MiB when memory is available; Desktop fixes the budget at 400 MiB; Low
fixes it at 50 MiB and reduces prefetch. Custom keeps manual cache limits and
uses the detected host class for prefetch. Editing the advanced cache keys
selects Custom. Selecting Auto restores defaults; no DAC restart is required.


The default playback cache uses both RAM (L1) and disk (L2). Machines with less
than 2 GiB of RAM automatically use a 50 MiB L1 budget; the normal budget is
400 MiB. Dynamic growth remains disabled by default. These budgets describe
cached compressed audio, not the process's total memory usage.

Tracks larger than the available L1 budget, or with an unknown download size,
stream into temporary files inside the playback cache directory. Playback,
seeking, resume and gapless preparation read these files in bounded chunks.
Completed L2 hits open file readers without loading the entire track into RAM.
Active readers survive cache eviction; their disk space is released when the
last reader closes. Active download files can temporarily exceed the persistent
L2 quota (800 MiB by default).

On the low-memory profile, qbzd skips speculative Hi-Res prefetch; it prepares
the next track at the requested quality when needed for gapless playback.
The explicitly selected **Streaming only** mode disables disk caching and
spooling, so its growing download buffers can still use substantial RAM.
Leave that mode off on a small-memory streamer. A missing or unwritable disk
cache produces an error for an oversized track instead of silently allocating
the full file in RAM.

## Why glibc 2.35

This binary is built on ubuntu-22.04 (glibc 2.35) specifically so it runs on
Raspberry Pi OS bookworm (glibc 2.36) and similarly-aged distros without a
rebuild — see `qbz-nix-docs/qbz-daemon/01-architecture.md` (D13).
