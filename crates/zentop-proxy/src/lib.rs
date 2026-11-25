//! In-memory traffic sessions, with a loopback HTTP front end and blind CONNECT.
mod attribution;
mod engine;
mod presentation;
mod store;

pub use engine::{Options, Session};
pub use presentation::{copy_as_curl, copy_body, preview, BodyPreview, Metadata};

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
}

impl FlowState {
    pub fn has_error(&self) -> bool {
        matches!(self, Self::Refused(_) | Self::Failed(_))
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
