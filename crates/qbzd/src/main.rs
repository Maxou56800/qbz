use clap::{Parser, Subcommand};

mod adapter;
mod api;
mod cli;
mod config;
mod daemon;
mod events_bridge;
mod hooks;
mod listen_log_engine;
mod lock;
mod login;
mod mpris;
mod paths;
mod qconnect;
mod scrobble_engine;
mod state;
mod tui;

pub const API_VERSION: u32 = 1; // 02-cli-and-api.md §1.6

#[derive(Parser)]
#[command(name = "qbzd", version, arg_required_else_help = true,
          about = "QBZ headless Qobuz playback daemon")]
struct Cli {
    /// Target daemon (default 127.0.0.1:8182; env QBZD_HOST)
    #[arg(long, global = true)]
    host: Option<String>,
    #[arg(short, long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground (systemd ExecStart)
    Run {
        /// Enable the experimental Orbit library API
        #[arg(long, hide = true)]
        orbit: bool,
    },
    /// Experimental host-owned local library (requires run --orbit)
    #[command(hide = true)]
    Library { #[command(subcommand)] cmd: LibraryCmd },
    /// Log in to Qobuz (one-shot browser listener; --paste; --token)
    Login {
        #[arg(long)] callback_host: Option<String>,
        #[arg(long)] paste: bool,
        #[arg(long)] token: Option<String>,
    },
    Logout,
    /// Interactive configurator (six screens)
    Setup,
    /// Composite daemon diagnostic
    Status { #[arg(long)] json: bool },
    Ping   { #[arg(long)] json: bool },
    /// One-line now-playing
    Now    { #[arg(long)] json: bool },
    /// Stream live daemon events (SSE); default = newline-delimited JSON
    Watch  { #[arg(long)] raw: bool },
    /// Search Qobuz — top hits with ids (--ids pipes into `queue add -`)
    Search {
        query: String,
        #[arg(long = "type", default_value = "all")] kind: String,
        #[arg(long, default_value_t = 20)] limit: u32,
        #[arg(long, default_value_t = 0)] offset: u32,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
    /// Album page — tracklist with ids
    Album {
        id: String,
        #[arg(long)] suggest: bool,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
    /// Artist page (default), --top tracks, or --albums grid
    Artist {
        id: u64,
        #[arg(long)] top: bool,
        #[arg(long)] albums: bool,
        #[arg(long, default_value_t = 20)] limit: u32,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
    /// Similar artists or albums: artist:ID | album:ID
    Similar {
        selector: String,
        #[arg(long, default_value_t = 20)] limit: u32,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
    /// For-You suggestions — seeds from the queue, or --seed <ID,ID>|-
    Suggest {
        #[arg(long)] seed: Option<String>,
        #[arg(long, default_value_t = 20)] limit: u32,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
    /// Discover rails: index | most-streamed | new-releases | press-awards |
    /// qobuzissims | album-of-the-week | ideal-discography | playlists | tags |
    /// release-watch (replicate Discover; no recommendations)
    Discover {
        section: Option<String>,
        #[arg(long)] genre: Option<String>,
        #[arg(long)] tag: Option<String>,
        #[arg(long = "release-type")] release_type: Option<String>,
        #[arg(long = "type")] kind: Option<String>,
        #[arg(long, default_value_t = 20)] limit: u32,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
    /// Recommendations: playlist <ID> (Suggested Songs — no history needed)
    Reco { #[command(subcommand)] cmd: RecoCmd },
    /// Favorites: list | add | remove
    Fav { #[command(subcommand)] cmd: FavCmd },
    /// Playlists: list | show
    Playlist { #[command(subcommand)] cmd: PlaylistCmd },
    /// Shuffle: on | off | toggle (bare = toggle)
    Shuffle { mode: Option<String> },
    /// Repeat: off | all | one
    Repeat  { mode: String },
    /// Seed-and-go radio: artist:ID | track:ID | album:ID
    Radio { seed: String },
    /// Lyrics for a track (bare = current); --synced adds [mm:ss.cc] timestamps
    Lyrics { track_id: Option<u64>, #[arg(long)] synced: bool, #[arg(long)] json: bool },
    /// Current-track cover art — prints the URL, or --save PATH downloads it
    Art { #[arg(long)] save: Option<String> },
    /// Resolve a Qobuz URL to a kind:ID token (pure, no daemon)
    Resolve { url: String },
    /// Resume (bare) or play content: album:ID | track:ID | artist:ID | playlist:ID | URL
    Play   { content: Option<String> },
    Pause, Toggle, Stop, Next, Prev,
    /// Absolute secs, +N/-N, or mm:ss
    Seek   { position: String },
    /// Bare = read; 0-100, +N, -N
    Volume { value: Option<String>, #[arg(long)] json: bool },
    /// Bare = toggle
    Mute   { state: Option<String> },
    Queue    { #[command(subcommand)] cmd: QueueCmd },
    Settings { #[command(subcommand)] cmd: SettingsCmd },
    Qconnect { #[command(subcommand)] cmd: QconnectCmd },
    /// Scrobbling: login (Last.fm / ListenBrainz) · status · enable · disable
    Scrobble { #[command(subcommand)] cmd: ScrobbleCmd },
    Config   { #[command(subcommand)] cmd: ConfigCmd },
    Version  { #[arg(long)] json: bool },
    /// Generate an init service file (systemd/openrc/runit); prints to stdout
    Service {
        /// systemd | openrc | runit (auto-detected from the running init if omitted)
        init: Option<String>,
        /// User the service runs as (default: current user)
        #[arg(long)] user: Option<String>,
        /// Path to the qbzd binary (default: this executable, else /usr/bin/qbzd)
        #[arg(long)] bin: Option<String>,
        /// systemd: emit a SYSTEM unit (runs as --user) instead of a user unit
        #[arg(long)] system: bool,
    },
    /// Shell completions (hidden; packaged by T14)
    #[command(hide = true)]
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum LibraryCmd {
    /// Inspect this host's file library
    Info,
    /// List directories registered on the target host
    Folders,
    /// Register an existing absolute directory on the target host
    Add { path: String },
    /// Queue a scan of one directory or the whole library
    Scan { #[arg(long)] folder: Option<i64> },
    /// Read scan progress and its current job id
    Jobs,
    /// Cancel this specific job (never a newer one)
    Cancel { job: u64 },
    /// Search the target host's indexed files
    Search { query: String, #[arg(long, default_value_t = 25)] limit: u32 },
}

#[derive(Subcommand)]
enum QueueCmd {
    List  { #[arg(long)] json: bool },
    Add   { track_id: u64, #[arg(long)] next: bool },
    Remove{ index: usize },
    Clear { #[arg(long)] keep_current: bool },
    /// Reorder a 1-based position to another
    Move  { from: usize, to: usize },
    /// Jump to (play) a 1-based position
    Jump  { position: usize },
    /// Stop after the current track (or `off` to clear)
    StopAfter { arg: Option<String> },
}

#[derive(Subcommand)]
enum RecoCmd {
    Playlist {
        id: u64,
        #[arg(long)] limit: Option<u32>,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
}

#[derive(Subcommand)]
enum FavCmd {
    List {
        #[arg(long = "type")] kind: Option<String>,
        #[arg(long)] ids: bool,
        #[arg(long)] json: bool,
    },
    Add    { fav_type: String, id: Option<String>, #[arg(long)] current: bool },
    Remove { fav_type: String, id: String },
}

#[derive(Subcommand)]
enum PlaylistCmd {
    List { #[arg(long)] json: bool },
    Show { id: u64, #[arg(long)] ids: bool, #[arg(long)] json: bool },
    /// Create a playlist
    Create { name: String, #[arg(long)] desc: Option<String>, #[arg(long)] public: bool },
    /// Rename / re-describe / change visibility
    Edit {
        id: u64,
        #[arg(long)] name: Option<String>,
        #[arg(long)] desc: Option<String>,
        #[arg(long)] public: bool,
        #[arg(long)] private: bool,
    },
    /// Delete an owned playlist (requires --yes)
    Rm { id: u64, #[arg(long)] yes: bool },
    /// Add tracks (ids, or - to read from stdin)
    Add { id: u64, track_ids: Vec<String> },
    /// Remove tracks (plain track ids)
    Remove { id: u64, track_ids: Vec<String> },
}

#[derive(Subcommand)]
enum SettingsCmd {
    Export {
        file: Option<String>,
        #[arg(long, default_value = "daemon")] from: String, // daemon|desktop
        #[arg(long)] include_auth: bool,
    },
    Import {
        file: String,
        #[arg(long)] include_auth: bool,
        #[arg(long)] trust_dsd: bool,
        #[arg(long)] remap: Vec<String>,   // OLD=NEW, repeatable
        #[arg(long)] dry_run: bool,
    },
    Show { #[arg(long)] json: bool },
    /// List canonical ALSA playback-volume controls for the selected route
    MixerControls { #[arg(long)] json: bool },
    Set  { key: String, #[arg(allow_negative_numbers = true)] value: String },
}

#[derive(Subcommand)]
enum QconnectCmd { Enable, Disable, Name { name: String } }

#[derive(Subcommand)]
enum ScrobbleCmd {
    /// Connect a provider
    Login { #[command(subcommand)] cmd: ScrobbleLoginCmd },
    /// Connection + enabled state
    Status,
    /// Stop scrobbling to a provider (keeps credentials)
    Disable { provider: String },
    /// Resume scrobbling to a provider
    Enable { provider: String },
}

#[derive(Subcommand)]
enum ScrobbleLoginCmd {
    /// Last.fm web auth (prints a URL to approve)
    Lastfm,
    /// ListenBrainz user token (from listenbrainz.org/settings)
    Listenbrainz { #[arg(long)] token: String },
}

// The tokenless default has no rotation verb (02 §3.1.2): `config` is just
// path|show. Rotating the opt-in [server] token = edit qbzd.toml + restart.
#[derive(Subcommand)]
enum ConfigCmd { Path, Show { #[arg(long)] json: bool } }

#[tokio::main]
async fn main() {
    // Install the rustls process-level `CryptoProvider`
    qbz_app::ensure_crypto_provider();

    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Version { json } => {
            if json {
                println!("{{\"version\":\"{}\",\"api_version\":{}}}",
                         env!("CARGO_PKG_VERSION"), API_VERSION);
            } else {
                println!("qbzd {} (api v{})", env!("CARGO_PKG_VERSION"), API_VERSION);
            }
            0
        }
        Cmd::Service { init, user, bin, system } => cli::service::service(init, user, bin, system),
        Cmd::Completions { shell } => {
            use clap::CommandFactory;
            clap_complete::generate(shell, &mut Cli::command(), "qbzd",
                                    &mut std::io::stdout());
            0
        }
        Cmd::Run { orbit } => {
            // Phase 1: resolve the config root and load qbzd.toml. The config's
            // `data_root` (a container override) can redirect the data/cache
            // roots, so resolve those in phase 2 once it is known.
            let bootstrap = paths::ProfileRoots::resolve(None, None);
            let cfg_path = bootstrap.config.join("qbzd.toml");
            let (cfg, warns) = match config::QbzdConfig::load(&cfg_path) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error: {e}");
                    eprintln!("  → fix or remove the config:  {}", cfg_path.display());
                    std::process::exit(1);
                }
            };
            // Phase 2: honor an explicit `data_root` container override.
            let data_root = cfg.data_root.clone();
            let roots = paths::ProfileRoots::resolve(
                None,
                data_root.as_deref().map(std::path::Path::new),
            );
            match daemon::run(roots, cfg, warns, orbit).await {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Library { cmd } => {
            let roots = login_roots();
            cli::library::run(cli.host, &roots, cmd).await
        }
        Cmd::Login {
            callback_host,
            paste,
            token,
        } => {
            let roots = login_roots();
            let result = if let Some(tok) = token {
                login::login_with_token_arg(&roots, &tok).await
            } else if paste {
                login::login_paste(&roots).await
            } else {
                login::login_browser(&roots, callback_host).await
            };
            match result {
                Ok(session) => {
                    println!("{}", cli::copy::login_success(&session));
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Logout => {
            let roots = login_roots();
            match login::logout(&roots) {
                Ok(daemon_nudged) => {
                    println!("{}", cli::copy::logout_success(daemon_nudged));
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Cmd::Status { json } => {
            // The CLI reads only local qbzd.toml (for the opt-in token); the
            // config root is always at its XDG default.
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::status::status(cli.host, json, &roots).await
        }
        Cmd::Ping { json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::status::ping(cli.host, json, &roots).await
        }
        Cmd::Now { json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::now(cli.host, json, &roots).await
        }
        Cmd::Watch { raw } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::watch::watch(cli.host, raw, &roots).await
        }
        Cmd::Search { query, kind, limit, offset, ids, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::search::search(cli.host, query, kind, limit, offset, ids, json, &roots).await
        }
        Cmd::Album { id, suggest, ids, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::browse::album(cli.host, id, suggest, ids, json, &roots).await
        }
        Cmd::Artist { id, top, albums, limit, ids, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::browse::artist(cli.host, id, top, albums, limit, ids, json, &roots).await
        }
        Cmd::Similar { selector, limit, ids, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::browse::similar(cli.host, selector, limit, ids, json, &roots).await
        }
        Cmd::Suggest { seed, limit, ids, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::browse::suggest(cli.host, seed, limit, ids, json, &roots).await
        }
        Cmd::Discover { section, genre, tag, release_type, kind, limit, ids, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::discover::discover(cli.host, section, genre, tag, release_type, kind, limit, ids, json, &roots).await
        }
        Cmd::Reco { cmd } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            match cmd {
                RecoCmd::Playlist { id, limit, ids, json } => {
                    cli::reco::playlist(cli.host, id, limit, ids, json, &roots).await
                }
            }
        }
        Cmd::Fav { cmd } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            match cmd {
                FavCmd::List { kind, ids, json } => cli::fav::list(cli.host, kind, ids, json, &roots).await,
                FavCmd::Add { fav_type, id, current } => {
                    cli::fav::add(cli.host, fav_type, id, current, &roots).await
                }
                FavCmd::Remove { fav_type, id } => cli::fav::remove(cli.host, fav_type, id, &roots).await,
            }
        }
        Cmd::Playlist { cmd } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            match cmd {
                PlaylistCmd::List { json } => cli::playlist::list(cli.host, json, &roots).await,
                PlaylistCmd::Show { id, ids, json } => {
                    cli::playlist::show(cli.host, id, ids, json, &roots).await
                }
                PlaylistCmd::Create { name, desc, public } => {
                    cli::playlist::create(cli.host, name, desc, public, &roots).await
                }
                PlaylistCmd::Edit { id, name, desc, public, private } => {
                    cli::playlist::edit(cli.host, id, name, desc, public, private, &roots).await
                }
                PlaylistCmd::Rm { id, yes } => cli::playlist::rm(cli.host, id, yes, &roots).await,
                PlaylistCmd::Add { id, track_ids } => {
                    cli::playlist::add(cli.host, id, track_ids, &roots).await
                }
                PlaylistCmd::Remove { id, track_ids } => {
                    cli::playlist::remove(cli.host, id, track_ids, &roots).await
                }
            }
        }
        Cmd::Shuffle { mode } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::mode::shuffle(cli.host, mode, &roots).await
        }
        Cmd::Repeat { mode } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::mode::repeat(cli.host, mode, &roots).await
        }
        Cmd::Radio { seed } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::radio::radio(cli.host, seed, &roots).await
        }
        Cmd::Lyrics { track_id, synced, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::lyrics::lyrics(cli.host, track_id, synced, json, &roots).await
        }
        Cmd::Art { save } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::art::art(cli.host, save, &roots).await
        }
        Cmd::Resolve { url } => cli::resolve::resolve(url),
        Cmd::Play { content } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::play::play(cli.host, content, &roots).await
        }
        Cmd::Pause => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::pause(cli.host, &roots).await
        }
        Cmd::Toggle => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::toggle(cli.host, &roots).await
        }
        Cmd::Stop => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::stop(cli.host, &roots).await
        }
        Cmd::Next => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::next(cli.host, &roots).await
        }
        Cmd::Prev => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::prev(cli.host, &roots).await
        }
        Cmd::Seek { position } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::seek(cli.host, &roots, position).await
        }
        Cmd::Volume { value, json } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::volume(cli.host, &roots, value, json).await
        }
        Cmd::Mute { state } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            cli::transport::mute(cli.host, &roots, state).await
        }
        Cmd::Queue { cmd } => {
            let roots = paths::ProfileRoots::resolve(None, None);
            match cmd {
                QueueCmd::List { json } => cli::queue::list(cli.host, json, &roots).await,
                QueueCmd::Add { track_id, next } => {
                    cli::queue::add(cli.host, &roots, track_id, next).await
                }
                QueueCmd::Remove { index } => cli::queue::remove(cli.host, &roots, index).await,
                QueueCmd::Clear { keep_current } => {
                    cli::queue::clear(cli.host, &roots, keep_current).await
                }
                QueueCmd::Move { from, to } => cli::queue::move_(cli.host, &roots, from, to).await,
                QueueCmd::Jump { position } => cli::queue::jump(cli.host, &roots, position).await,
                QueueCmd::StopAfter { arg } => cli::queue::stop_after(cli.host, &roots, arg).await,
            }
        }

        Cmd::Settings { cmd } => {
            let roots = login_roots();
            match cmd {
                SettingsCmd::Show { json } => cli::settings::show(json, &roots),
                SettingsCmd::MixerControls { json } => cli::settings::mixer_controls(json, &roots),
                SettingsCmd::Set { key, value } => cli::settings::set(&roots, &key, &value),
                SettingsCmd::Export {
                    file,
                    from,
                    include_auth,
                } => cli::settings::export(&roots, file, &from, include_auth),
                SettingsCmd::Import {
                    file,
                    include_auth,
                    trust_dsd,
                    remap,
                    dry_run,
                } => {
                    cli::settings::import(&roots, &file, include_auth, trust_dsd, &remap, dry_run)
                        .await
                }
            }
        }
        Cmd::Qconnect { cmd } => {
            let roots = login_roots();
            match cmd {
                QconnectCmd::Enable => cli::settings::qconnect_enable(&roots),
                QconnectCmd::Disable => cli::settings::qconnect_disable(&roots),
                QconnectCmd::Name { name } => cli::settings::qconnect_name(&roots, &name),
            }
        }
        Cmd::Scrobble { cmd } => {
            let roots = login_roots();
            match cmd {
                ScrobbleCmd::Login { cmd } => match cmd {
                    ScrobbleLoginCmd::Lastfm => cli::scrobble::login_lastfm(cli.host, &roots).await,
                    ScrobbleLoginCmd::Listenbrainz { token } => {
                        cli::scrobble::login_listenbrainz(cli.host, token, &roots).await
                    }
                },
                ScrobbleCmd::Status => cli::scrobble::status(&roots),
                ScrobbleCmd::Disable { provider } => {
                    cli::scrobble::set_enabled(cli.host, provider, false, &roots).await
                }
                ScrobbleCmd::Enable { provider } => {
                    cli::scrobble::set_enabled(cli.host, provider, true, &roots).await
                }
            }
        }
        Cmd::Config { cmd } => {
            let roots = login_roots();
            match cmd {
                ConfigCmd::Path => cli::settings::config_path(&roots),
                ConfigCmd::Show { json } => cli::settings::config_show(json, &roots),
            }
        }
        Cmd::Setup => {
            // The setup TUI edits the daemon's REAL stores at the daemon roots,
            // honoring a `qbzd.toml` `data_root` override exactly like `run`.
            let roots = login_roots();
            tui::run(roots).await
        }
    };
    log::logger().flush();
    std::process::exit(code);
}

/// Resolve the daemon profile roots for a local CLI auth operation. `login` and
/// `logout` write the credential file into the config root and nudge the LOCAL
/// daemon, so — like `run` — they honor a `qbzd.toml` `data_root` override while
/// keeping the config root at its XDG default.
fn login_roots() -> paths::ProfileRoots {
    let bootstrap = paths::ProfileRoots::resolve(None, None);
    let cfg_path = bootstrap.config.join("qbzd.toml");
    let data_root = config::QbzdConfig::load(&cfg_path)
        .ok()
        .and_then(|(c, _)| c.data_root);
    paths::ProfileRoots::resolve(None, data_root.as_deref().map(std::path::Path::new))
}

#[cfg(test)]
mod command_line_tests {
    use super::*;

    #[test]
    fn settings_accepts_negative_numbers_without_swallowing_options() {
        for value in ["-14", "-14.5", "0", "true"] {
            let cli = Cli::try_parse_from([
                "qbzd", "settings", "set", "audio.normalization_target_lufs", value,
            ]).unwrap();
            assert!(matches!(cli.cmd, Cmd::Settings {
                cmd: SettingsCmd::Set { value: parsed, .. }
            } if parsed == value));
        }
        assert!(Cli::try_parse_from([
            "qbzd", "settings", "set", "audio.normalization_target_lufs", "--typo",
        ]).is_err());
    }
}
