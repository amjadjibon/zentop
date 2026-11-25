//! In-memory traffic sessions with a loopback HTTP proxy, CONNECT tunnels, and optional TLS decryption.
mod attribution;
mod content;
mod decryption;
mod engine;
mod har;
pub use decryption::{canonical_host, decrypt_host, skip_reason, CertificateAuthority, Decryption};
#[cfg(target_os = "macos")]
pub mod keychain;
mod presentation;
mod replay;
mod store;
mod websocket;

pub use content::ContentKind;
pub use engine::{Options, Session};
pub use har::{HarExport, HarOptions};
pub use presentation::{copy_as_curl, copy_body, preview, BodyPreview, Metadata};
pub use replay::{ReplayDraft, ReplayEdits, ReplayHeader, ReplayValue};
pub use websocket::{Direction, Message, Opcode};

use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;

const PENDING_ATTRIBUTION: Attribution = Attribution::Unknown("Attribution pending");

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum Attribution {
    Known {
        unique_id: u64,
        pid: u32,
        start_time: u64,
        name: String,
    },
    Unknown(&'static str),
}

impl Attribution {
    fn unknown(reason: &'static str) -> Self {
        Self::Unknown(reason)
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Known { name, .. } => name,
            Self::Unknown(_) => "Unknown process",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    Http,
    Tunnel,
    Upgrade,
    DecryptedHttps,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum FlowState {
    Active,
    Complete,
    Refused(&'static str),
    Failed(&'static str),
    UpstreamTls(String),
}

impl FlowState {
    pub fn error_reason(&self) -> Option<&str> {
        match self {
            Self::Refused(reason) | Self::Failed(reason) => Some(reason),
            Self::UpstreamTls(reason) => Some(reason),
            Self::Active | Self::Complete => None,
        }
    }

    pub fn has_error(&self) -> bool {
        self.error_reason().is_some()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Timing {
    pub attribution: Option<Duration>,
    pub connect: Option<Duration>,
    pub tls: Option<Duration>,
    pub ttfb: Option<Duration>,
    pub total: Duration,
}

impl Serialize for Timing {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Milliseconds {
            attribution_ms: Option<f64>,
            connect_ms: Option<f64>,
            tls_ms: Option<f64>,
            ttfb_ms: Option<f64>,
            total_ms: f64,
        }

        let ms = |duration: Duration| duration.as_secs_f64() * 1_000.;
        Milliseconds {
            attribution_ms: self.attribution.map(ms),
            connect_ms: self.connect.map(ms),
            tls_ms: self.tls.map(ms),
            ttfb_ms: self.ttfb.map(ms),
            total_ms: ms(self.total),
        }
        .serialize(serializer)
    }
}

/// Coalesced row changes, or a replacement after attachment or queue overflow.
pub enum RowUpdates {
    Snapshot(Vec<Metadata>),
    Changed(Vec<(u64, Option<Metadata>)>),
}

pub type Headers = Vec<(String, String)>;

#[derive(Clone, Debug)]
pub struct RequestHead {
    pub method: String,
    pub url: String,
    pub host: String,
    pub path: String,
    pub version: String,
    pub headers: Headers,
}

#[derive(Clone, Debug)]
pub struct ResponseHead {
    pub status: u16,
    pub version: String,
    pub headers: Headers,
}

#[derive(Clone, Debug)]
pub enum Body {
    Captured(Arc<Vec<u8>>),
    Truncated { kept: Arc<Vec<u8>>, total: u64 },
    Skipped(&'static str),
}

impl Default for Body {
    fn default() -> Self {
        Self::Captured(Arc::default())
    }
}

impl Body {
    pub fn kept(&self) -> &[u8] {
        match self {
            Self::Captured(bytes) | Self::Truncated { kept: bytes, .. } => bytes,
            Self::Skipped(_) => &[],
        }
    }
}

/// Projected detail for one selected flow. The store does not expose raw heads
/// unless the caller explicitly requests secrets for that flow.
#[derive(Debug)]
pub struct Flow {
    pub metadata: Metadata,
    pub request: RequestHead,
    pub response: Option<ResponseHead>,
    pub request_body: Body,
    pub response_body: Body,
    pub headers_truncated: bool,
    pub replayable: bool,
    pub messages: Vec<Arc<Message>>,
    pub message_count: u64,
    pub websocket: bool,
    /// The request's `Authorization` scheme, such as `Bearer`. The scheme is
    /// not a credential, so it is shown even while the value is masked.
    pub auth_scheme: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub refused: u64,
    pub flows_dropped: u64,
    pub unattributed: u64,
    pub evicted_bodies: u64,
    pub sent: u64,
    pub received: u64,
    pub memory: usize,
    pub paused: bool,
    pub allow_local_destinations: bool,
}
