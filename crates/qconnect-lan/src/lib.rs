//! Official Qobuz Connect LAN surface, both halves.
//!
//! Receiver half: local discovery (announce), HTTP wire validation and bounded
//! admission. Controller half (`controller`, 2026-09-13): browse, probe and
//! hand delegated credentials to another receiver. The crate deliberately has
//! no Qobuz client, player, Qt or daemon dependency; credential validation and
//! activation belong to the coordinator, and minting the delegated tokens to
//! `qbz-qobuz` (`delegate_qconnect_auth`).

mod admission;
mod controller;
mod mdns;
mod model;
mod projection;
mod server;
mod validation;

pub use admission::{admission_channel, AdmissionInbox, AdmissionSender, SubmitError};
pub use controller::{
    endpoint_url, ordered_addresses, HandoffBody, LanBrowseEvent, LanBrowser,
    LanControllerClient, LanControllerError, LanRendererCandidate, LanRendererProbe,
    LanTokenOut,
};
pub use model::{
    ConnectInfo, DeviceType, DisplayInfo, HandoffCandidate, LanJwtToken, MaxAudioQuality,
};
pub use projection::LanProjection;
pub use server::{
    LanError, LanHttpMethod, LanHttpRoute, LanRequestObservation, LanService, LanServiceConfig,
    SERVICE_TYPE,
};
pub use validation::{EndpointPolicy, ValidationError, MAX_BODY_BYTES};
