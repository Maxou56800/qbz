#!/usr/bin/env bash
# QBZ per-user installer. Canonical source: vicrodh/qbz scripts/install.sh.
# Published verbatim at https://qbz.lol/install.sh with the 2.1.2 website.
# Keep execution after the complete function definition: a truncated download
# must not start installing half a script. Compatible with macOS Bash 3.2.
qbz_install_main() (
    set -euo pipefail
    umask 077
    local requested='' check_only=0 action=install arg
    for arg in "$@"; do
        case "$arg" in
            --help|-h)
                printf '%s\n' 'QBZ installer — Linux and macOS, x86_64 and ARM64' \
                    'Usage: bash install.sh [--check] [--version=X.Y.Z] [--uninstall]' \
                    'Default: latest stable release (2.1.2 or newer), installed for this user.' \
                    '--check: resolve and validate release metadata without installing.' \
                    '--uninstall: remove only this installer’s application and launchers; keep your data.'
                return 0 ;;
            --check) check_only=1 ;;
            --uninstall) action=uninstall ;;
            --version=*) requested=${arg#--version=} ;;
            *) printf 'Unknown argument: %s\n' "$arg" >&2; return 1 ;;
        esac
    done
    fail() { printf 'QBZ: %s\n' "$*" >&2; exit 1; }
    if [ "$action" = uninstall ] && { [ "$check_only" = 1 ] || [ -n "$requested" ]; }; then
        fail '--uninstall cannot be combined with --check or --version.'
    fi
    [ "${EUID:-$(id -u)}" -ne 0 ] || fail 'Run as your normal user, without sudo.'
    [ -n "${HOME:-}" ] && [ "${HOME#/}" != "$HOME" ] && [ "$HOME" != / ] || fail 'HOME must be an absolute user directory.'
    # Desktop Exec and shell launchers cannot safely represent control characters.
    case "$HOME" in *$'\n'*|*$'\r'*|*$'\t'*) fail 'HOME contains an unsupported control character.' ;; esac
    local system arch platform install_dir destination receipt launcher desktop icon
    system=$(uname -s)
    arch=$(uname -m)
    case "$arch" in x86_64|amd64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;; *) fail "Unsupported architecture: $arch" ;; esac
    case "$system" in
        Linux)
            local libc libc_major libc_minor floor=35
            [ "$arch" != aarch64 ] || floor=39
            libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) || fail 'The official Linux package requires glibc (musl systems are not supported).'
            libc=${libc#glibc }
            IFS=. read -r libc_major libc_minor <<< "$libc"
            [[ "$libc_major" =~ ^[0-9]+$ ]] && [[ "$libc_minor" =~ ^[0-9]+$ ]] || fail 'Could not determine the glibc version.'
            (( libc_major > 2 || (libc_major == 2 && libc_minor >= floor) )) || fail "This architecture requires glibc 2.$floor or newer. Use your distribution's package instead."
            platform="linux-$arch"
            install_dir="$HOME/.local/opt/qbz"
            destination="$install_dir/QBZ.AppImage"
            receipt="$install_dir/.qbz-installer"
            launcher="$HOME/.local/bin/qbz"
            desktop="$HOME/.local/share/applications/com.blitzfc.qbz.desktop"
            icon="$install_dir/qbz.png" ;;
        Darwin)
            # Prefer a native bundle even when invoked from a Rosetta terminal.
            if [ "$(/usr/sbin/sysctl -n hw.optional.arm64 2>/dev/null || :)" = 1 ]; then arch=aarch64; fi
            platform="darwin-$arch"
            install_dir="$HOME/Applications"
            destination="$install_dir/QBZ.app"
            receipt="$install_dir/.qbz-installer"
            launcher=''; desktop=''; icon='' ;;
        *) fail "Unsupported operating system: $system" ;;
    esac
    [ ! -L "$install_dir" ] && [ ! -L "$destination" ] && [ ! -L "$receipt" ] || fail 'Installation path is a symlink; manage it through its original installer.'
    if [ "$action" = uninstall ]; then
        if command -v pgrep >/dev/null 2>&1 && pgrep -x qbz >/dev/null 2>&1; then fail 'Close QBZ before uninstalling it.'; fi
        [ -f "$receipt" ] && [ "$(cat "$receipt")" = 'qbz-one-line-v1' ] || fail 'This installation is not owned by this script.'
        if [ "$system" = Linux ]; then
            for arg in "$launcher" "$desktop"; do
                if [ -f "$arg" ] && [ ! -L "$arg" ] && grep -q '^# QBZ one-line installer$' "$arg"; then rm -- "$arg"; fi
            done
            rm -f -- "$destination" "$icon" "$receipt"
            rmdir "$install_dir" 2>/dev/null || :
        else
            [ -d "$destination" ] || fail 'QBZ.app is missing.'
            /usr/bin/codesign -d --verbose=2 "$destination" 2>&1 | grep -q '^Signature=adhoc$' || fail 'The macOS signing channel changed; remove the app through its current installer.'
            rm -rf -- "$destination"
            rm -- "$receipt"
        fi
        printf '%s\n' 'QBZ removed. Your settings, library and downloads were kept.'
        return 0
    fi
    if [ "$check_only" = 0 ]; then
        if [ -e "$destination" ]; then
            if [ -f "$receipt" ] && [ "$(cat "$receipt")" = 'qbz-one-line-v1' ]; then
                printf '%s\n' 'QBZ is already installed. Use Settings → Updates in QBZ.'
                return 0
            fi
            fail "An existing installation occupies $destination. Use its original updater."
        fi
        if [ "$system" = Linux ]; then
            for arg in "$launcher" "$desktop"; do
                [ ! -e "$arg" ] && [ ! -L "$arg" ] || fail "Existing launcher preserved: $arg"
            done
        fi
        # A second install would shadow a distribution package or signed Mac app.
        if command -v qbz >/dev/null 2>&1; then fail 'QBZ is already on PATH. Update it through its original installer.'; fi
        if [ "$system" = Darwin ] && [ -e /Applications/QBZ.app ]; then fail 'QBZ already exists in /Applications. Keep its original update channel.'; fi
        if [ "$system" = Linux ]; then
            if command -v flatpak >/dev/null 2>&1 && flatpak info com.blitzfc.qbz >/dev/null 2>&1; then fail 'QBZ is installed with Flatpak. Update it through Flatpak.'; fi
            if command -v snap >/dev/null 2>&1 && snap list qbz-player >/dev/null 2>&1; then fail 'QBZ is installed with Snap. Snap manages its updates.'; fi
        fi
    fi
    command -v curl >/dev/null 2>&1 || fail 'curl is required.'
    if ! command -v python3 >/dev/null 2>&1 && ! command -v jq >/dev/null 2>&1 && [ "$system" != Darwin ]; then
        fail 'Install python3 or jq with your package manager, then run this command again.'
    fi
    local scratch
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/qbz-install.XXXXXXXX")
    # A private scratch directory is the only recursively cleaned temporary path.
    trap 'rm -rf -- "$scratch"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    fetch() { curl --proto '=https' --proto-redir '=https' --tlsv1.2 --fail --location --silent --show-error --connect-timeout 10 --max-time 600 --max-filesize 2147483648 --retry 2 --output "$2" "$1"; }
    local api='https://api.github.com/repos/vicrodh/qbz/releases/latest'
    if [ -n "$requested" ]; then
        [[ "$requested" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail '--version must be a stable X.Y.Z version.'
        api="https://api.github.com/repos/vicrodh/qbz/releases/tags/v$requested"
    fi
    printf '%s\n' 'Finding the latest stable QBZ package...'
    fetch "$api" "$scratch/release.json" || fail 'Cannot read the release. Check your connection or GitHub rate limit.'
    # Parse JSON as data. No eval, shell interpolation or regex JSON parsing.
    json_get() {
        # macOS ships JXA. Prefer it to Apple's python3 stub, which can trigger
        # a Command Line Tools installation on an otherwise stock machine.
        if [ "$system" = Darwin ] && [ -x /usr/bin/osascript ]; then
            /usr/bin/osascript -l JavaScript - "$scratch/release.json" "$1" <<'JS'
ObjC.import('Foundation');
function run(args) {
    const data = $.NSData.dataWithContentsOfFile(args[0]);
    let value = JSON.parse(ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding)));
    args[1].split('.').forEach(function (key) { value = value[key]; });
    if (Array.isArray(value)) return String(value.length);
    if (['string', 'boolean', 'number'].indexOf(typeof value) < 0) throw Error('Unsupported release field');
    return String(value);
}
JS
        elif command -v python3 >/dev/null 2>&1; then
            python3 - "$scratch/release.json" "$1" <<'PY'
import json,sys
v=json.load(open(sys.argv[1]))
for key in sys.argv[2].split('.'): v=v[int(key)] if isinstance(v,list) else v[key]
if isinstance(v,list): print(len(v))
elif isinstance(v,bool): print(str(v).lower())
elif isinstance(v,(str,int)): print(v)
else: raise SystemExit('Unsupported release field')
PY
        else
            jq -er --arg path "$1" 'getpath($path | split(".") | map(tonumber? // .)) | if type == "array" then length elif type == "boolean" then tostring else . end' "$scratch/release.json"
        fi
    }
    local tag version major minor patch filename url='' digest='' size='' count i name
    tag=$(json_get tag_name)
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'The release is not a stable version.'
    [ "$(json_get draft)" = false ] && [ "$(json_get prerelease)" = false ] || fail 'Pre-release installation is not supported.'
    version=${tag#v}
    IFS=. read -r major minor patch <<< "$version"
    if (( 10#$major < 2 || (10#$major == 2 && 10#$minor < 1) || (10#$major == 2 && 10#$minor == 1 && 10#$patch < 2) )); then
        fail 'The one-line installer requires QBZ 2.1.2 or newer. It will become available with that release.'
    fi
    [ -z "$requested" ] || [ "$version" = "$requested" ] || fail 'The response does not match the requested version.'
    case "$platform" in
        linux-x86_64) filename="QBZ_${version}_amd64.AppImage" ;;
        linux-aarch64) filename="QBZ_${version}_aarch64.AppImage" ;;
        darwin-x86_64) filename='QBZ_x64.app.tar.gz' ;;
        darwin-aarch64) filename='QBZ_aarch64.app.tar.gz' ;;
    esac
    count=$(json_get assets)
    [[ "$count" =~ ^[0-9]+$ ]] && [ "$count" -le 1000 ] || fail 'Invalid asset count.'
    for ((i=0; i<count; i++)); do
        name=$(json_get "assets.$i.name")
        if [ "$name" = "$filename" ]; then
            [ -z "$url" ] || fail 'The release contains duplicate packages.'
            url=$(json_get "assets.$i.browser_download_url")
            digest=$(json_get "assets.$i.digest")
            size=$(json_get "assets.$i.size")
        fi
    done
    [ "$url" = "https://github.com/vicrodh/qbz/releases/download/$tag/$filename" ] || fail "The official $platform package is not available yet. Try again after its release build finishes."
    [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] || fail 'The release has no SHA-256 digest. Refusing an unchecked download.'
    [[ "$size" =~ ^[0-9]+$ ]] && [ "$size" -gt 0 ] && [ "$size" -le 2147483648 ] || fail 'Invalid package size.'
    printf 'QBZ %s · %s\nDestination: %s\n' "$version" "$platform" "$destination"
    if [ "$check_only" = 1 ]; then printf 'Package: %s\nSHA-256: %s\n' "$url" "${digest#sha256:}"; return 0; fi
    fetch "$url" "$scratch/package" || fail 'Download failed. No installation was changed.'
    local actual
    if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$scratch/package"); else actual=$(shasum -a 256 "$scratch/package"); fi
    [ "${actual%% *}" = "${digest#sha256:}" ] || fail 'Package checksum mismatch. No installation was changed.'
    [ "$(wc -c < "$scratch/package" | tr -d ' ')" = "$size" ] || fail 'Package size mismatch.'
    mkdir -p "$install_dir"
    mkdir "$install_dir/.qbz-setup.lock" 2>/dev/null || fail 'Another installer is active (or left .qbz-setup.lock after interruption).'
    trap 'rm -rf -- "$scratch"; rmdir "$install_dir/.qbz-setup.lock"' EXIT
    [ ! -e "$destination" ] && [ ! -L "$destination" ] || fail 'An installation appeared while downloading. It was preserved.'
    local stage
    stage=$(mktemp -d "$install_dir/.qbz-setup.XXXXXXXX")
    local committed=0 published=("$stage")
    trap 'if [ "$committed" = 0 ]; then for arg in "${published[@]}"; do rm -rf -- "$arg"; done; fi; rm -rf -- "$scratch" "$stage"; rmdir "$install_dir/.qbz-setup.lock"' EXIT
    if [ "$system" = Linux ]; then
        for arg in "$launcher" "$desktop"; do
            [ ! -e "$arg" ] && [ ! -L "$arg" ] || fail "A launcher appeared while downloading: $arg"
        done
        [ "$(od -An -tx1 -N4 "$scratch/package" | tr -d ' \n')" = 7f454c46 ] || fail 'The package is not an ELF executable.'
        [ "$(od -An -tx1 -j8 -N3 "$scratch/package" | tr -d ' \n')" = 414902 ] || fail 'The package is not a type-2 AppImage.'
        cp "$scratch/package" "$stage/QBZ.AppImage"
        chmod 755 "$stage/QBZ.AppImage"
        # The logo is data from the same website that hosts this script.
        fetch 'https://qbz.lol/assets/brand/64x64.png' "$stage/qbz.png" || fail 'Could not download the application icon.'
        mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
        {
            printf '%s\n' '#!/usr/bin/env bash' '# QBZ one-line installer'
            # Extract-and-run works without FUSE and preserves APPIMAGE for the updater.
            printf 'export APPIMAGE_EXTRACT_AND_RUN=1\nexec %q "$@"\n' "$destination"
        } > "$stage/launcher"
        chmod 755 "$stage/launcher"
        local desktop_exec desktop_icon
        desktop_exec=${launcher//\\/\\\\}; desktop_exec=${desktop_exec//\"/\\\"}; desktop_exec=${desktop_exec//\$/\\$}; desktop_exec=${desktop_exec//\`/\\\`}; desktop_exec=${desktop_exec//%/%%}
        # Desktop Entry string escaping happens before Exec argument unquoting.
        desktop_exec=${desktop_exec//\\/\\\\}
        desktop_icon=${icon//\\/\\\\}
        printf '# QBZ one-line installer\n[Desktop Entry]\nType=Application\nName=QBZ\nComment=Qobuz hi-res player\nExec="%s" %%U\nIcon=%s\nTerminal=false\nCategories=AudioVideo;Audio;Player;\nStartupWMClass=qbz\n' "$desktop_exec" "$desktop_icon" > "$stage/desktop"
        chmod 644 "$stage/desktop" "$stage/qbz.png"
        # Publish the executable last. All downloads and verification precede this.
        mv "$stage/qbz.png" "$icon"
        published+=("$icon")
        mv "$stage/launcher" "$launcher"
        published+=("$launcher")
        mv "$stage/desktop" "$desktop"
        published+=("$desktop")
        mv "$stage/QBZ.AppImage" "$destination"
        published+=("$destination")
    else
        mkdir "$stage/unpacked"
        tar -tzf "$scratch/package" > "$stage/contents"
        while IFS= read -r name; do
            case "$name" in QBZ.app|QBZ.app/*) ;; *) fail 'The archive contains files outside QBZ.app.' ;; esac
            case "/$name/" in */../*) fail 'The archive contains a parent path.' ;; esac
        done < "$stage/contents"
        tar -xzf "$scratch/package" -C "$stage/unpacked"
        [ -f "$stage/unpacked/QBZ.app/Contents/MacOS/qbz" ] && [ -f "$stage/unpacked/QBZ.app/Contents/Info.plist" ] || fail 'The archive is not a QBZ app bundle.'
        /usr/bin/codesign --verify --deep --strict "$stage/unpacked/QBZ.app" || fail 'The app bundle failed code-signature validation.'
        mv "$stage/unpacked/QBZ.app" "$destination"
        published+=("$destination")
    fi
    printf '%s\n' 'qbz-one-line-v1' > "$receipt"
    committed=1
    printf '%s\n' 'Installed. Open QBZ from your applications menu.' 'Future updates: Settings → Updates in QBZ.'
    if [ "$system" = Linux ]; then
        case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) printf 'For terminal launches, add %s to PATH. The menu launcher already works.\n' "$HOME/.local/bin" ;; esac
    fi
)
qbz_install_main "$@"
