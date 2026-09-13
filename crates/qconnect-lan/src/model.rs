use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

// `Deserialize` (2026-09-13) is the controller half reading another
// renderer's answers; a value this build does not know collapses to
// `Unknown` instead of failing the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaxAudioQuality {
    MP3,
    #[serde(rename = "UP_TO_CD")]
    UpToCd,
    #[serde(rename = "UP_TO_HIRES_96")]
    UpToHires96,
    #[serde(rename = "UP_TO_HIRES_192")]
    UpToHires192,
    #[serde(rename = "UP_TO_HIRES_384")]
    UpToHires384,
    #[serde(rename = "UNKNOWN", other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    Phone,
    Speaker,
    Streamer,
    TV,
    Soundbar,
    Computer,
    Headset,
    Tablet,
    GoogleCast,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub friendly_name: String,
    pub serial_number: String,
    pub brand_display_name: String,
    pub model_display_name: String,
    pub max_audio_quality: MaxAudioQuality,
    #[serde(rename = "type")]
    pub device_type: DeviceType,
    /// Optional on the wire (Electron carries it, Android does not).
    #[serde(default)]
    pub software_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectInfo {
    pub app_id: String,
    pub current_session_id: Option<String>,
}

/// Raw official LAN token shape. Deliberately has no `Debug` implementation.
#[derive(Deserialize)]
pub(crate) struct RawLanJwtToken {
    pub endpoint: String,
    pub exp: i64,
    pub jwt: String,
}

impl Drop for RawLanJwtToken {
    fn drop(&mut self) {
        self.endpoint.zeroize();
        self.jwt.zeroize();
    }
}

/// Raw official handoff body. Deliberately has no `Debug` implementation.
#[derive(Deserialize)]
pub(crate) struct RawHandoffRequest {
    pub session_id: String,
    pub jwt_api: RawLanJwtToken,
    pub jwt_qconnect: RawLanJwtToken,
    pub become_active: bool,
}

impl Drop for RawHandoffRequest {
    fn drop(&mut self) {
        self.session_id.zeroize();
    }
}

/// Validated delegated token. Access is borrow-only so Drop always scrubs the
/// original token and endpoint buffers.
pub struct LanJwtToken {
    endpoint: String,
    exp: i64,
    jwt: String,
}

impl LanJwtToken {
    pub(crate) fn new(endpoint: String, exp: i64, jwt: String) -> Self {
        Self { endpoint, exp, jwt }
    }

    fn empty() -> Self {
        Self::new(String::new(), 0, String::new())
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub const fn expires_at(&self) -> i64 {
        self.exp
    }

    pub fn jwt(&self) -> &str {
        &self.jwt
    }

    /// Consume the token without cloning either sensitive allocation. The
    /// returned endpoint and JWT become the caller's zeroization responsibility.
    pub fn into_parts(mut self) -> (String, i64, String) {
        (
            std::mem::take(&mut self.endpoint),
            self.exp,
            std::mem::take(&mut self.jwt),
        )
    }
}

impl Drop for LanJwtToken {
    fn drop(&mut self) {
        self.endpoint.zeroize();
        self.jwt.zeroize();
    }
}

/// A structurally and locally validated handoff admitted for cloud validation.
/// Deliberately has no `Debug`, `Serialize` or `Clone` implementation.
pub struct HandoffCandidate {
    session_id: String,
    jwt_api: LanJwtToken,
    jwt_qconnect: LanJwtToken,
    become_active: bool,
}

impl HandoffCandidate {
    pub(crate) fn new(
        session_id: String,
        jwt_api: LanJwtToken,
        jwt_qconnect: LanJwtToken,
        become_active: bool,
    ) -> Self {
        Self {
            session_id,
            jwt_api,
            jwt_qconnect,
            become_active,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub const fn api_token(&self) -> &LanJwtToken {
        &self.jwt_api
    }

    pub const fn qconnect_token(&self) -> &LanJwtToken {
        &self.jwt_qconnect
    }

    pub const fn become_active(&self) -> bool {
        self.become_active
    }

    /// Consume the complete handoff without duplicating credential buffers.
    pub fn into_parts(mut self) -> (String, LanJwtToken, LanJwtToken, bool) {
        (
            std::mem::take(&mut self.session_id),
            std::mem::replace(&mut self.jwt_api, LanJwtToken::empty()),
            std::mem::replace(&mut self.jwt_qconnect, LanJwtToken::empty()),
            self.become_active,
        )
    }
}

impl Drop for HandoffCandidate {
    fn drop(&mut self) {
        self.session_id.zeroize();
    }
}
