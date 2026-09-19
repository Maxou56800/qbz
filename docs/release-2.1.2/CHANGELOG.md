# QBZ 2.1.2

Second maintenance release on the 2.1 Qt line, and the largest so far: 162
commits since 2.1.1. The headline is configurability — playback memory
profiles, rebuilt volume normalization and a single Storage section — followed
by a Qobuz Connect hardening round driven by real sessions with the official
clients, and a wide quality-of-life pass.

## Playback memory & storage

- Playback memory profiles in Settings > Playback: Auto, High, Desktop, Low and
  Custom. The profile drives the cache budget, startup buffers and prefetch;
  Auto picks Desktop or Low from the computer's RAM; High grows to 1600 MiB
  when memory is available and shrinks under pressure; Low uses 50 MiB and
  disk-backed buffers for large tracks.
- Streaming only disables caching entirely, like the official clients, and
  greys out the profile controls while it is on.
- New Storage section groups every on-disk cache (artwork, lyrics, Plex) with a
  configurable artwork budget, enforced during use and at startup.
- Shared image-cache eviction, lost with the Slint frontend, is restored and
  bounded; orphans are swept.
- The offline cache is purged from the Offline Cache Manager only, behind a
  tooltip and an explicit modal.

## Audio

- Volume normalization rebuilt on the loudness values Qobuz already ships:
  toggle, target-loudness presets and a clipping guard that only limits a
  boost. The gain is known before the first sample; the analyser applies it
  once and glides. The loudness cache stores absolute LUFS with peak and source.
- A-B loop from the player bar's + menu.
- Opt-in ±10 s skip buttons beside Previous and Next, with a buffered-edge clamp.
- S16 and S24 samples are encoded on exact integer scales.
- The output device is released at quit with late playback fenced; paused
  outputs are released and resumable sources rebuilt; a suspended DAC is
  recovered before a route change; sinks vacated by direct ALSA reservations are
  recovered.
- The audio thread never panics when the loudness cache cannot open; zero sample
  rates and empty channel masks from codec params are rejected.

## Qobuz Connect

- Renderer targets align when the session carries autoplay, so track changes
  from a controller work with autoplay on.
- A reconnect announces the configured device name instead of the default one,
  and renderer UUIDs survive ADD/UPDATE topology events.
- The local-queue takeover is confirmed by its own action uuid: reconnecting
  while QBZ was playing no longer leaves the authority fence armed silently
  discarding every controller command.
- A negative `queue_item_id` reads as an absent occurrence, so the last row of
  any queue is reachable from a controller again.
- The acknowledgement report names the commanded occurrence instead of omitting
  the ids (partial fix; edge cases remain open).
- The final insertion slot of a queue drag is preserved.
- A locked output no longer advertises remote volume nor reports volume/mute it
  does not apply (qbzd).
- Renderers on the local network are listed and paired from the Connect panel.

## Library, playlists & navigation

- Shift ranges and Ctrl groups on every select-mode click target: library rows,
  Explorer columns, folder rail, My QBZ, offline manager, artist album sections
  and the library folder table; Ctrl+A and Escape reach the offline manager.
- "Where you left off" reopens the exact page, its arguments and its tab.
- Search boxes behave the same everywhere: Escape clears and drops focus, a
  clear cross, the header box grows with the window, the sidebar filters follow
  the same contract, and the playlist editor's folder select is searchable.
- Track rows play on double click, never on a single click (#790).
- "Date added" sorts by real dates; playlist sorts order instead of reversing
  (#782).
- Move a playlist to a folder from its editor (#785); delete a playlist from any
  context menu (#776); remove tracks in bulk; save the queue as a playlist with
  local or offline rows.
- Queue and Listen list row menus offer Go to album and Go to artist.
- Rows slide into place instead of the list being rebuilt (keyed list model);
  an album leaving the library slides the grid.
- Settings sub-navigation regrouped under an Advanced fold, with new glyphs.
- Header nav goes compact only when the text tabs no longer fit.

## Catalog & metadata

- Featured artists on track rows, each name its own link, on by default.
- Buy on Qobuz: the catalog's purchasable flag on albums and tracks, label links
  from the Releases list and album menus, and an unavailable-release page with
  ranked alternatives.
- Track info and Album info are selectable and copy basic or full text; Copy
  submenus open on hover and hold their parent open.
- Placeholder catalog names never render as artists or composers.

## Appearance

- Two Wallpaper background modes: the desktop's picture or your own image, with
  configurable blur. On Plasma Wayland the wallpaper follows the window and its
  blurred twin drifts.
- Theme options sit under their picker, boxed and collapsible.
- Elevated controls lift on hover instead of darkening; ambient mode reaches
  every chrome control; accent buttons no longer turn black under the pointer.
- The player bar is one chrome band like the header, keeping its gutter (8 px,
  wider on macOS).

## Media servers

- Jellyfin 12 authentication and 10.8-10.10 delta compatibility.
- Album metadata editing from the album header.
- A saved Jellyfin session stays visible after a server test.
- Read timeout on the media-server proxy client; the Tidal/proxy fast path has
  a timeout and a shared client.
- An orphan "(migrated)" copy is reused instead of creating a second one (#502).

## Platform & packaging

- macOS: reopening hardened, miniplayer appearance fixed.
- Qt 6.8 and 6.9: QbzArrayModel loads and renders on both.
- Native update channels and one-line installation.
- Qt release checks and signed installation restored.

## Privacy & logs

- Media-server API keys and the stored Jellyfin token are redacted.
- Signed CDN URLs are kept out of CMAF fetch errors; CDN signature parameters
  are redacted.
- A log file another live process is writing is never rotated; qbzd writes to
  its own `qbzd.log` (#749).

## Performance

- The search-ranking store flushes off the calling thread; the write-only search
  artist cache is no longer persisted; accents and punctuation are folded in
  `normalize_query`.
- The local-library worker slot is released before joining on shutdown.
- Discord presence backs off for five minutes after a failed connect.
