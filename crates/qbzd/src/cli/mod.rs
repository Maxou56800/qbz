// crates/qbzd/src/cli/mod.rs — human-facing CLI presentation.
//
// The CLI is a stateless renderer (02-cli-and-api.md §1.1); the copy strings it
// prints are normative (§1.4 error voice, §2.2 per-verb output). Keeping them in
// one place lets the spec and the code diff cleanly.
pub mod art;
pub mod browse;
pub mod client;
pub mod copy;
pub mod discover;
pub mod fav;
pub mod lyrics;
pub mod mode;
pub mod play;
pub mod playlist;
pub mod queue;
pub mod radio;
pub mod reco;
pub mod resolve;
pub mod scrobble;
pub mod search;
pub mod service;
pub mod settings;
pub mod status;
pub mod transport;
pub mod watch;

pub mod library;
