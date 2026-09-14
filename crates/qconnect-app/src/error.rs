use qconnect_core::PendingActionError;
use qconnect_protocol::ProtocolError;
use qconnect_transport_ws::WsTransportError;
use thiserror::Error;

/// Stable, payload-free diagnostics for owner-authority failures that cross
/// the QConnect renderer boundary.
///
/// Owner API and playback errors can contain signed URLs, credential-bearing routing
/// material, or server-controlled response text. Frontend adapters must first
/// classify those errors into this enum and may only format this value.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum QconnectOwnerFailure {
    #[error("owner service is unavailable")]
    Unavailable,
    #[error("owner network request failed")]
    Network,
    #[error("owner authentication failed")]
    Authentication,
    #[error("owner authorization failed")]
    Authorization,
    #[error("owner service is offline")]
    Offline,
    #[error("owner request was rate limited")]
    RateLimited,
    #[error("owner server request failed")]
    Server,
    #[error("owner service returned an invalid response")]
    InvalidResponse,
    #[error("owner track is unavailable")]
    TrackUnavailable,
    #[error("owner playback failed")]
    Playback,
    #[error("owner internal operation failed")]
    Internal,
}

impl QconnectOwnerFailure {
    /// Collapse a legacy playback `String` without inspecting or retaining it.
    /// The player layer predates typed errors and may have embedded a signed URL.
    pub const fn from_opaque_playback_error(_error: &str) -> Self {
        Self::Playback
    }
}

#[derive(Debug, Error)]
pub enum QconnectAppError {
    #[error("renderer command could not be applied")]
    RendererExecution,
    #[error(transparent)]
    Pending(#[from] PendingActionError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Transport(#[from] WsTransportError),
}
