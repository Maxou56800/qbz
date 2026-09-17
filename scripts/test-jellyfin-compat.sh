#!/usr/bin/env bash
# Jellyfin cross-version compatibility lab (#502).
#
# Self-hosted servers sit on stable distributions and lag behind releases, so
# QBZ must keep working against old servers while following new ones. For
# every version below this starts a disposable Jellyfin container bound to
# 127.0.0.1, completes the startup wizard through the API, serves a synthetic
# 24-bit / 96 kHz FLAC library with an embedded cover, and runs the REAL
# protocol suites against it:
#
#   cargo test -p qbz-jellyfin --test live            (auth, views, items,
#                                                     quality, direct stream,
#                                                     anonymous cover, delta)
#   cargo test -p qbz-source --test live_remote jellyfin   (sweep -> cache ->
#                                                     playback ticket -> audio)
#
# It also records, for information only, whether the server still accepts the
# legacy `X-Emby-Token` header (12.0+ does not; QBZ must never depend on it).
#
# Usage:  scripts/test-jellyfin-compat.sh [version ...]
# Env:    QBZ_JELLYFIN_LAB   work directory (default: $TMPDIR/qbz-jellyfin-compat)
#         KEEP_IMAGES=1      keep the pulled images afterwards
# Needs:  docker, ffmpeg, python3, curl, cargo. Opt-in: not part of CI.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

versions=("$@")
if [[ ${#versions[@]} -eq 0 ]]; then
  versions=(10.8.13 10.9.11 10.10.7 10.11.11 12.1)
fi
lab="${QBZ_JELLYFIN_LAB:-${TMPDIR:-/tmp}/qbz-jellyfin-compat}"
user=qbzlab
pass=qbzlab-pass
container=""

cleanup() {
  if [[ -n "$container" ]]; then
    docker rm -f "$container" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

json() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

make_library() {
  local album="$lab/music/Lab Artist/Lab Album"
  [[ -f "$album/03 - Lab Track 3.flac" ]] && return
  mkdir -p "$album"
  ffmpeg -loglevel error -y -f lavfi -i "testsrc2=s=600x600" -frames:v 1 "$album/cover.png"
  for n in 1 2 3; do
    ffmpeg -loglevel error -y \
      -f lavfi -i "sine=frequency=$((330 * n)):duration=20:sample_rate=96000" \
      -i "$album/cover.png" -map 0:a -map 1:v \
      -ac 2 -sample_fmt s32 -c:a flac -bits_per_raw_sample 24 -c:v copy \
      -disposition:v attached_pic \
      -metadata title="Lab Track $n" -metadata artist="Lab Artist" \
      -metadata album_artist="Lab Artist" -metadata album="Lab Album" \
      -metadata track="$n" -metadata date=2026 -metadata genre=Test \
      "$album/0$n - Lab Track $n.flac"
  done
}

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

wait_ready() {
  local base="$1"
  for _ in $(seq 1 150); do
    if curl -s -m 2 "$base/System/Info/Public" | grep -q '"Version"'; then
      return 0
    fi
    sleep 2
  done
  return 1
}

# Called from an `if`, where `set -e` does not abort: every step that must
# stop the version returns explicitly.
run_version() {
  local version="$1" port base data setup auth token uid tracks legacy
  port="$(free_port)"
  base="http://127.0.0.1:$port"
  data="$lab/servers/$version"
  rm -rf "$data"
  mkdir -p "$data/config" "$data/cache"
  container="qbz-jellyfin-compat-${version//./-}"
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker pull -q "jellyfin/jellyfin:$version" >/dev/null || return 1
  docker run -d --name "$container" --user "$(id -u):$(id -g)" \
    -p "127.0.0.1:$port:8096" -v "$data/config:/config" -v "$data/cache:/cache" \
    -v "$lab/music:/music:ro" "jellyfin/jellyfin:$version" >/dev/null || return 1
  wait_ready "$base" || { echo "server never became ready"; return 1; }

  setup='Authorization: MediaBrowser Client="qbz-compat", Device="lab", DeviceId="qbz-compat-setup", Version="1"'
  curl -sf -o /dev/null -X POST "$base/Startup/Configuration" -H "$setup" \
    -H 'Content-Type: application/json' \
    -d '{"UICulture":"en-US","MetadataCountryCode":"US","PreferredMetadataLanguage":"en"}' || return 1
  curl -sf -o /dev/null "$base/Startup/User" -H "$setup" || return 1
  curl -sf -o /dev/null -X POST "$base/Startup/User" -H "$setup" \
    -H 'Content-Type: application/json' -d "{\"Name\":\"$user\",\"Password\":\"$pass\"}" || return 1
  curl -sf -o /dev/null -X POST "$base/Startup/Complete" -H "$setup" || return 1

  auth='Client="qbz-compat", Device="lab", DeviceId="qbz-compat-admin", Version="1"'
  local login
  login="$(curl -sf -X POST "$base/Users/AuthenticateByName" -H "Authorization: MediaBrowser $auth" \
    -H 'Content-Type: application/json' -d "{\"Username\":\"$user\",\"Pw\":\"$pass\"}")" || return 1
  token="$(json 'd["AccessToken"]' <<<"$login")" || return 1
  uid="$(json 'd["User"]["Id"]' <<<"$login")" || return 1
  auth="Authorization: MediaBrowser $auth, Token=\"$token\""
  curl -sf -o /dev/null -X POST \
    "$base/Library/VirtualFolders?name=Music&collectionType=music&refreshLibrary=true" \
    -H "$auth" -H 'Content-Type: application/json' \
    -d '{"LibraryOptions":{"PathInfos":[{"Path":"/music"}]}}' || return 1
  # Stage 1: the three tracks with media info. Stage 2: the album cover —
  # 12.x attaches it after the first scan, so a refresh is requested once.
  local items='/Items?userId='"$uid"'&IncludeItemTypes=Audio&Recursive=true&Fields=MediaSources'
  tracks=0
  for _ in $(seq 1 90); do
    tracks="$(curl -s "$base$items" -H "$auth" \
      | json 'sum(1 for i in d.get("Items", []) if i.get("MediaSources"))' 2>/dev/null || echo 0)"
    [[ "$tracks" == 3 ]] && break
    sleep 2
  done
  [[ "$tracks" == 3 ]] || { echo "library scan incomplete ($tracks/3)"; return 1; }
  local covered=0 album refreshed=0
  for _ in $(seq 1 60); do
    covered="$(curl -s "$base$items" -H "$auth" \
      | json 'sum(1 for i in d.get("Items", []) if i.get("AlbumPrimaryImageTag"))' 2>/dev/null || echo 0)"
    [[ "$covered" == 3 ]] && break
    if [[ "$refreshed" == 0 ]]; then
      album="$(curl -s "$base/Items?userId=$uid&IncludeItemTypes=MusicAlbum&Recursive=true" -H "$auth" \
        | json 'd["Items"][0]["Id"]' 2>/dev/null || true)"
      if [[ -n "$album" ]]; then
        curl -s -o /dev/null -X POST \
          "$base/Items/$album/Refresh?metadataRefreshMode=FullRefresh&imageRefreshMode=FullRefresh&replaceAllImages=true" \
          -H "$auth"
        refreshed=1
      fi
    fi
    sleep 2
  done
  [[ "$covered" == 3 ]] || { echo "album cover never attached ($covered/3)"; return 1; }
  legacy="$(curl -s -o /dev/null -w '%{http_code}' "$base/Users/$uid/Views" -H "X-Emby-Token: $token")"
  echo "legacy X-Emby-Token -> HTTP $legacy (information only)"

  QBZ_JELLYFIN_URL="$base" QBZ_JELLYFIN_USER="$user" QBZ_JELLYFIN_PASS="$pass" \
    cargo test --manifest-path crates/Cargo.toml -p qbz-jellyfin --test live -- --test-threads=1 || return 1
  QBZ_JELLYFIN_URL="$base" QBZ_JELLYFIN_USER="$user" QBZ_JELLYFIN_PASS="$pass" \
    cargo test --manifest-path crates/Cargo.toml -p qbz-source --test live_remote jellyfin || return 1
}

mkdir -p "$lab"
make_library
declare -a summary=()
failed=0
for version in "${versions[@]}"; do
  echo "=== Jellyfin $version"
  if run_version "$version"; then
    summary+=("PASS  $version")
  else
    summary+=("FAIL  $version")
    failed=1
  fi
  cleanup
  container=""
  if [[ "${KEEP_IMAGES:-0}" != 1 ]]; then
    docker rmi "jellyfin/jellyfin:$version" >/dev/null 2>&1 || true
  fi
done
printf '%s\n' "=== Jellyfin compatibility" "${summary[@]}"
exit "$failed"
