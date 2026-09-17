//! Shared HTTP infrastructure and catalog handlers for QBZ hosts.
//!
//! The host owns playback, settings and session authority. This crate owns
//! HTTP access control, wire framing and operations over an explicit core.
//! No desktop globals or daemon state enter this boundary.

pub mod artwork;
pub mod browse;
pub mod discover;
pub mod fav;
pub mod library;
pub mod lyrics;
pub mod playlist;
pub mod reco;
mod routes;
pub mod search;
mod server;
mod sse;
mod wire;

pub use routes::{P0_ROUTES, P1_ROUTES};
pub use server::{bind, serve, ApiHandle, BindError, BoundServer, HttpHost};
pub use wire::{canon_volume, err_json, error_body, json};

/// Borrowed, request-scoped catalog view. The host snapshots its session gate
/// before routing; no global current-profile lookup occurs inside a handler.
pub struct CatalogContext<'a, A: qbz_models::FrontendAdapter> {
    pub core: &'a qbz_core::QbzCore<A>,
    pub rt: &'a tokio::runtime::Handle,
    pub needs_auth: bool,
}
