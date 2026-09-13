#!/usr/bin/env bash
# Publish the four AUR packages (qbz-bin, qbz, qbzd-bin, qbzd) for a released
# version — the manual last step of the release, made one command.
#
#   scripts/packaging/aur_publish.sh <version> [--push] [--pkgrel N]
#
# Without --push it PREPARES: stamps packaging/aur/* with the version and the
# sha256 of every published asset (downloaded from its final release URL —
# never a local "equivalent" file), regenerates .SRCINFO in a clean Arch
# container, clones the four AUR repos into a work directory and commits
# "Update to <version>" in each, with the ssh push URL set. With --push it
# then pushes each repo; the maintainer's key passkey is asked interactively.
#
# Work directory: $AUR_PUBLISH_WORK_DIR (default /mnt/build/artifacts/aur-<version>).
# Needs: curl, git, docker (for makepkg --printsrcinfo), ssh access to AUR for --push.
set -euo pipefail

version="${1:-}"
if [[ -z "$version" ]]; then
  echo "usage: $0 <version> [--push] [--pkgrel N]" >&2
  exit 2
fi
shift
push=false
pkgrel=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --push) push=true ;;
    --pkgrel) pkgrel="$2"; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "AUR publishing is limited to stable x.y.z releases (got $version)" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="${AUR_PUBLISH_WORK_DIR:-/mnt/build/artifacts/aur-${version}}"
sources="$work/sources"
mkdir -p "$sources"
base="https://github.com/vicrodh/qbz/releases/download/v${version}"

download_hash() {
  local url="$1" file="$2"
  [[ -s "$sources/$file" ]] || curl -fsSL "$url" -o "$sources/$file"
  sha256sum "$sources/$file" | cut -d' ' -f1
}
echo "[aur] downloading the published v${version} assets into $sources"
qbz_amd64="$(download_hash "$base/qbz_${version}_amd64.tar.gz" qbz-amd64.tar.gz)"
qbz_aarch64="$(download_hash "$base/qbz_${version}_aarch64.tar.gz" qbz-aarch64.tar.gz)"
qbzd_amd64="$(download_hash "$base/qbzd-${version}-linux-amd64.tar.gz" qbzd-amd64.tar.gz)"
qbzd_aarch64="$(download_hash "$base/qbzd-${version}-linux-aarch64.tar.gz" qbzd-aarch64.tar.gz)"
vendor="$(download_hash "$base/qbz-${version}-cargo-vendor.tar.xz" vendor.tar.xz)"
source_sha="$(download_hash "https://github.com/vicrodh/qbz/archive/refs/tags/v${version}.tar.gz" source.tar.gz)"

for package in qbz-bin qbz qbzd-bin qbzd; do
  dir="$work/$package"
  if [[ ! -d "$dir/.git" ]]; then
    rm -rf "$dir"
    git clone -q "https://aur.archlinux.org/${package}.git" "$dir"
  fi
  git -C "$dir" remote set-url --push origin "ssh://aur@aur.archlinux.org/${package}.git"
  cp "$repo_root/packaging/aur/${package}/"* "$dir/"
  sed -i "s/^pkgver=.*/pkgver=${version}/" "$dir/PKGBUILD"
  sed -i "s/^pkgrel=.*/pkgrel=${pkgrel}/" "$dir/PKGBUILD"
done
sed -i "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('${qbz_amd64}')/" "$work/qbz-bin/PKGBUILD"
sed -i "s/^sha256sums_aarch64=.*/sha256sums_aarch64=('${qbz_aarch64}')/" "$work/qbz-bin/PKGBUILD"
sed -i "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('${qbzd_amd64}')/" "$work/qbzd-bin/PKGBUILD"
sed -i "s/^sha256sums_aarch64=.*/sha256sums_aarch64=('${qbzd_aarch64}')/" "$work/qbzd-bin/PKGBUILD"
sed -i "s/^sha256sums=.*/sha256sums=('${source_sha}' '${vendor}')/" "$work/qbz/PKGBUILD"
sed -i "s/^sha256sums=.*/sha256sums=('${source_sha}' '${vendor}')/" "$work/qbzd/PKGBUILD"

echo "[aur] regenerating .SRCINFO in a clean Arch container"
for package in qbz-bin qbz qbzd-bin qbzd; do
  docker run --rm -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$work/$package:/pkg" archlinux:latest bash -euc '
      useradd -m builder
      chown -R builder:builder /pkg
      runuser -u builder -- bash -c "cd /pkg && makepkg --printsrcinfo > .SRCINFO"
      chown -R "${HOST_UID}:${HOST_GID}" /pkg
    ' >/dev/null
done

for package in qbz-bin qbz qbzd-bin qbzd; do
  dir="$work/$package"
  git -C "$dir" add -A
  if git -C "$dir" diff --cached --quiet; then
    echo "[aur] ${package}: already at ${version}-${pkgrel}, nothing to commit"
    continue
  fi
  git -C "$dir" commit -q -m "Update to ${version}"
  echo "[aur] ${package}: $(git -C "$dir" log --oneline -1)"
done

if [[ "$push" != true ]]; then
  echo "[aur] prepared in $work — review, then rerun with --push (the AUR key passkey is asked per repo)"
  exit 0
fi
for package in qbz-bin qbz qbzd-bin qbzd; do
  echo "[aur] pushing ${package}"
  git -C "$work/$package" push origin master
done
echo "[aur] done — check https://aur.archlinux.org/packages?K=qbz"
