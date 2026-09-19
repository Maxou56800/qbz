# 2.1.2 — Rebuild Q (You Can (Not) Redo)

Second maintenance release for the 2.1 line, and the biggest one so far: 162 commits.

The headline is that QBZ stops deciding for you. Playback memory is now a profile you choose — Auto, High, Desktop, Low or Custom — so the app rarely goes near a gigabyte of RAM unless you want it to, and Streaming only turns the cache off entirely. Volume normalization was rebuilt on the loudness values Qobuz already ships, with presets for the common cases, and every on-disk cache now lives in one Storage section with a budget you set.

Qobuz Connect got a hardening round driven by real sessions with the official clients: track changes with autoplay in the session, reconnects that kept the device's name, a stuck authority fence that silently dropped every command, and the last row of a queue that no controller could reach.

The rest is a wide quality-of-life pass — Excel-style multi-select everywhere, the app reopening exactly where you left it, consistent search boxes, A-B loop and skip buttons — plus a long list of fixes across audio, the library, media servers and both desktop platforms.

Feliz día de la independencia desde México! 🇲🇽   
<small>(Si, bueno este release tendría que haber salido ayer, pero no sucedió)</small>

---

## New

  - **Playback memory profiles** — choose Auto, High, Desktop, Low or Custom in Settings > Playback; the cache budget, the startup buffers and prefetch follow the profile, Auto picks from the computer's RAM, and Streaming only turns caching off like the official clients do (this last was there for a long time, but people isn't using it, so this is just a friendly reminder).
  - **Volume normalization** — Settings > Audio > Loudness: the normalization toggle, target-loudness presets and a clipping guard that only limits a boost, never attenuates. The gain is known before the first sample and the analyser applies it once.
  - **Storage section** — every on-disk cache in one place, with a configurable artwork budget that is enforced during use, swept at startup and cleaned of orphans.
  - **Wallpaper backgrounds** — two modes, your desktop picture or your own image, with configurable blur; on Plasma Wayland the wallpaper follows the window and its blurred twin drifts.
  - **Where you left off** — reopens the exact page, its arguments and its tab.
  - **Excel-style multi-select** — Shift ranges and Ctrl groups on every surface: library, Explorer columns, folder rail, My QBZ, offline manager, artist sections and the folder table; Ctrl+A and Escape reach them too.
  - **A-B loop** — from the player bar's + menu, plus opt-in ±10 s skip buttons beside Previous and Next.
  - **Featured artists** — "feat." performers on track rows, each name its own link, on by default.
  - **Buy on Qobuz** — the catalog's purchasable flag on albums and tracks, label links from the Releases list and album menus, and an unavailable-release page with ranked alternatives.
  - **Selectable info** — Track info and Album info are selectable, copy basic or full text, and Copy submenus open on hover.
  - **Qobuz Connect on your network** — renderers on the local network are listed and paired from the Connect panel.
  - **Settings, reorganized** — the sub-navigation is regrouped under an Advanced fold with new glyphs, and the offline cache is purged from the manager only, behind a tooltip and an explicit modal.
  - **Playlists** — move a playlist to a folder from its editor (#785), delete one from any context menu (#776), remove tracks in bulk, and save a queue as a playlist with local or offline rows.
  - **Update channels** — native update channels and one-line installation.
  - **Media servers** — Jellyfin 12 authentication and 10.8-10.10 compatibility, plus album metadata editing from the album header.

---

## Fixes

  - **Audio device release** — the output is released at quit with late playback fenced, paused outputs are freed and resumable sources rebuilt, a suspended DAC is recovered before a route change, and sinks vacated by direct ALSA reservations are recovered.
  - **Bit-perfect encoding** — S16 and S24 samples are encoded on exact integer scales.
  - **Qobuz Connect** — track changes work when the session carries autoplay; a reconnect announces the device name you configured; the local-queue takeover confirms by its own action uuid instead of leaving every controller command discarded; a negative `queue_item_id` reads as an absent occurrence, so the last row of a queue is reachable again; renderer UUIDs survive topology events; the final insertion slot of a drag is preserved; the acknowledgement report names the commanded occurrence; and a locked output no longer advertises or fakes remote volume. The conflict fence that guards a local takeover is released in the shared session loop too, so no host can leave it armed (#795, Maxou56800).
  - **Track rows** — a single click never plays; the disc and the row body follow one rule (#790).
  - **Caching** — masters obtained at the requested quality are reused instead of re-downloaded, and acquisition quality survives bounded audio storage.
  - **Search** — accents and punctuation are folded in queries, the ranking store flushes off the calling thread, and the write-only artist cache is gone.
  - **Library** — "Date added" sorts by real dates, playlist sorts order instead of reversing (#782), an album leaving the library slides the grid instead of rebuilding it, and folders holding SACD images show their selection in the rail.
  - **Memory** — the shared image cache eviction lost with the Slint frontend is restored and bounded, and the local-library worker slot is released before joining on shutdown.
  - **Privacy** — media-server API keys and the stored Jellyfin token are redacted, signed CDN URLs are kept out of CMAF errors, and CDN signature parameters are redacted.
  - **Logs** — a log file another live process is writing is never rotated; qbzd writes to its own `qbzd.log` (#749).
  - **macOS** — reopening is hardened, the miniplayer's appearance is fixed, and the player bar keeps its 8 px gutter.
  - **Qt 6.8 / 6.9** — QbzArrayModel loads and renders on both.
  - **UI polish** — modal forms open clean, elevated controls lift on hover instead of darkening, ambient mode reaches every chrome control, submenus stay open under the pointer, list rebuilds no longer steal the search box's focus, accent buttons no longer turn black under the pointer, and placeholder catalog names never render as artists.
  - **Media servers** — a saved Jellyfin session stays visible after a server test, the proxy client has a read timeout, and an orphan "(migrated)" copy is reused instead of duplicated (#502).
  - **Integrations** — Discord presence backs off for five minutes after a failed connect, and the audio thread never panics when the loudness cache cannot open.

---

## Notes

  - The 2.1.2 What's New opens once per version and can be reopened from the hamburger menu at any time.
  - Playback memory profiles apply to new buffers immediately; "Apply now" releases the existing ones.
  - Streaming only disables the memory profile controls while it is on.

---

Thanks to everyone who reported, tested and kept the feedback coming — this release is built on it.

**Full changelog:** https://github.com/vicrodh/qbz/compare/v2.1.1...v2.1.2
