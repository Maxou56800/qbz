{
  description = "QBZ — Native hi-fi Qobuz desktop player for Linux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    let
      # ──────────────────────────────────────────────
      # VERSION BUMP: bump `qbzVersion` and the per-arch tarball hashes AFTER a
      # tag publishes its release assets (hashes are computed from the published
      # asset at its final URL — see below).
      #
      # This flake installs the prebuilt Qt binary tarball — the SAME bare-binary
      # asset AUR-bin / Gentoo / Flathub repack — instead of compiling the Rust
      # + Qt workspace. That matches the project's binary-repack packaging
      # doctrine and sidesteps the cxx-qt / qt-build-utils source build entirely
      # (github #744: `Could not find qmltyperegistrar`, which needs Qt host
      # tools from several outputs on PATH at build time).
      # ──────────────────────────────────────────────
      qbzVersion = "2.1.2";

      # Bare-binary release tarballs, per arch. Layout inside each:
      #   qbz_<ver>_<arch>/{qbz, qbzd, LICENSE, licenses/, qbz.desktop, icons/}
      # Hashes: `nix-prefetch-url <url>` then `nix hash convert --to sri`.
      assets = {
        "x86_64-linux" = {
          name = "qbz_${qbzVersion}_amd64.tar.gz";
          hash = "sha256-cvbjBM4g+Q300GsSXFELaBUdLN+ej1Q3dxdpRZhOhN8=";
        };
        "aarch64-linux" = {
          name = "qbz_${qbzVersion}_aarch64.tar.gz";
          hash = "sha256-DNYD/Lbw68Cvl5rhtTmr7bzrEqwR6LM0RP6cXaRP5UA=";
        };
      };
    in
    flake-utils.lib.eachSystem (builtins.attrNames assets) (system:
      let
        pkgs = import nixpkgs { inherit system; };
        asset = assets.${system};

        # Opened by name at runtime rather than linked; wrapQtAppsHook already
        # covers Qt's own plugin/QML closure.
        runtimeLibs = with pkgs; [ libjack2 ];
        runtimeBins = with pkgs; [ pipewire pulseaudio xdg-utils ];
      in
      {
        packages.default = pkgs.stdenv.mkDerivation {
          pname = "qbz";
          version = qbzVersion;

          src = pkgs.fetchurl {
            url = "https://github.com/vicrodh/qbz/releases/download/v${qbzVersion}/${asset.name}";
            inherit (asset) hash;
          };

          # autoPatchelfHook rewrites the prebuilt binary's interpreter + rpath
          # against the nixpkgs closure; wrapQtAppsHook sets the Qt plugin / QML
          # import paths the binary needs at run time.
          nativeBuildInputs = with pkgs; [
            autoPatchelfHook
            qt6.wrapQtAppsHook
          ];

          # qt6-base/declarative/svg/wayland mirror the AUR `qbz-bin` depends;
          # the rest satisfy the bare binary's DT_NEEDED (ALSA, xkbcommon,
          # wayland, zlib, libstdc++) plus the dlopen'd JACK backend.
          buildInputs = with pkgs; [
            alsa-lib
            qt6.qtbase
            qt6.qtdeclarative
            qt6.qtsvg
            qt6.qtwayland
            libxkbcommon
            wayland
            zlib
            stdenv.cc.cc.lib
          ] ++ runtimeLibs;

          # Only the desktop client — `qbzd` ships as its own package elsewhere,
          # matching the source flake's previous scope.
          installPhase = ''
            runHook preInstall

            install -Dm755 qbz "$out/bin/qbz"
            install -Dm644 qbz.desktop \
              "$out/share/applications/com.blitzfc.qbz.desktop"
            # KWin matches the Exec path against /proc/<pid>/exe before it
            # offers org_kde_plasma_window_management (Wallpaper background).
            substituteInPlace "$out/share/applications/com.blitzfc.qbz.desktop" \
              --replace-warn "Exec=/usr/bin/qbz" "Exec=$out/bin/qbz"
            if [ -d icons ]; then cp -r icons "$out/share/icons"; fi
            install -Dm644 LICENSE "$out/share/licenses/qbz/LICENSE"
            if [ -d licenses ]; then
              cp -r licenses "$out/share/licenses/qbz/third-party"
            fi

            runHook postInstall
          '';

          # Fold the runtime helpers/libraries QBZ opens by name into the SAME
          # wrap wrapQtAppsHook performs (no double wrapper).
          preFixup = ''
            qtWrapperArgs+=(--prefix PATH : ${pkgs.lib.makeBinPath runtimeBins})
            qtWrapperArgs+=(--prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibs})
          '';

          meta = with pkgs.lib; {
            description = "Native, full-featured hi-fi Qobuz desktop player for Linux";
            homepage = "https://qbz.lol";
            downloadPage = "https://github.com/vicrodh/qbz/releases";
            license = licenses.mit;
            mainProgram = "qbz";
            platforms = [ "x86_64-linux" "aarch64-linux" ];
            sourceProvenance = [ sourceTypes.binaryNativeCode ];
          };
        };

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/qbz";
        };

        # Running a locally-compiled debug binary (crates/target/debug/qbz)
        # needs the dlopen'd JACK backend on LD_LIBRARY_PATH, same as the
        # wrapped package. Rust itself comes from the contributor's rustup.
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ rust-analyzer rustfmt clippy ];
          shellHook = ''
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
          '';
        };
      });
}
