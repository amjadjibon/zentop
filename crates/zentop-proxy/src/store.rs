use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use http::HeaderMap;

use crate::content::Classifier;
use crate::presentation::{self, Metadata, Secrets};
use crate::{
    Attribution, Body, Counters, Flow, FlowKind, FlowState, Headers, RequestHead, ResponseHead,
    Timing, PENDING_ATTRIBUTION,
};

const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const FLOW_LIMIT: usize = 2_000;
const BODY_LIMIT: usize = 1024 * 1024;
const HEAD_LIMIT: usize = 64 * 1024;
const EVENT_LIMIT: usize = 1_024;
const MESSAGE_OVERHEAD: usize =
    std::mem::size_of::<crate::Message>() + 3 * std::mem::size_of::<usize>();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    Request,
    Response,
}

struct Record {
    metadata: Metadata,
    request: RequestHead,
    response: Option<ResponseHead>,
    private_url: Option<String>,
    private_headers: [Vec<(usize, String)>; 2],
    bodies: [Body; 2],
    headers_truncated: bool,
    request_headers_truncated: bool,
    request_complete: bool,
    request_has_trailers: bool,
    opaque_request_headers: Vec<(usize, http::HeaderValue)>,
    head_bytes: usize,
    messages: Vec<Arc<crate::Message>>,
    message_count: u64,
    message_payload: usize,
    message_work: usize,
    messages_cleared: bool,
    websocket: bool,
    classifier: Classifier,
    reported: bool,
    revision: u64,
}

#[derive(Default)]
struct CapturedHeaders {
    display: Headers,
    private: Vec<(usize, String)>,
    bytes: usize,
    truncated: bool,
    opaque: Vec<(usize, http::HeaderValue)>,
}

pub(crate) struct Store {
    pub counters: Counters,
    pub revision: u64,
    next_id: u64,
    records: BTreeMap<u64, Record>,
    bodies: VecDeque<(u64, Side)>,
    events: VecDeque<u64>,
    events_overflowed: bool,
    event_pending: std::collections::HashSet<u64>,
    memory_limit: usize,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            counters: Counters::default(),
            revision: 0,
            next_id: 0,
            records: BTreeMap::new(),
            bodies: VecDeque::new(),
            events: VecDeque::new(),
            events_overflowed: false,
            event_pending: Default::default(),
            memory_limit: MEMORY_LIMIT,
        }
    }
}

impl Store {
    fn changed(&mut self, id: u64) {
        self.revision += 1;
        if let Some(record) = self.records.get_mut(&id) {
            record.revision = self.revision;
        }
        if self.event_pending.contains(&id) {
            return;
        }
        if self.events.len() == EVENT_LIMIT {
            if let Some(old) = self.events.pop_front() {
                self.event_pending.remove(&old);
                self.events_overflowed = true;
            }
        }
        self.events.push_back(id);
        self.event_pending.insert(id);
    }

    pub fn drain_events(&mut self) -> Vec<u64> {
        self.event_pending.clear();
        self.events_overflowed = false;
        self.events.drain(..).collect()
    }

    pub fn refuse(&mut self) {
        self.counters.refused += 1;
        self.revision += 1;
    }

    pub fn pause(&mut self, paused: bool) {
        self.counters.paused = paused;
        self.revision += 1;
        if paused {
            // A body that spans a pause must never stitch noncontiguous bytes together.
            let mut changed = Vec::new();
            for record in self.records.values_mut() {
                if record.metadata.state != FlowState::Active {
                    continue;
                }
                changed.push(record.metadata.id);
                for (body, total) in record
                    .bodies
                    .iter_mut()
                    .zip([record.metadata.bytes.0, record.metadata.bytes.1])
                {
                    if let Body::Captured(kept) = body {
                        *body = Body::Truncated {
                            kept: std::mem::take(kept),
                            total,
                        };
                    }
                }
            }
            for id in changed {
                self.changed(id);
            }
        }
    }

    fn remove(&mut self, id: u64) {
        if let Some(record) = self.records.remove(&id) {
            self.counters.memory -= record.head_bytes
                + record.message_payload
                + record.messages.len() * MESSAGE_OVERHEAD
                + record
                    .bodies
                    .iter()
                    .map(|body| body.kept().len())
                    .sum::<usize>();
            self.bodies.retain(|(owner, _)| *owner != id);
            self.changed(id);
        }
    }

    fn skip_body(&mut self, id: u64, side: Side, reason: &'static str) {
        if let Some(record) = self.records.get_mut(&id) {
            let body = &mut record.bodies[side as usize];
            self.counters.memory -= body.kept().len();
            *body = Body::Skipped(reason);
            self.changed(id);
        }
    }

    pub fn clear(&mut self) {
        let ids: Vec<_> = self.records.keys().copied().collect();
        self.bodies.clear();
        for id in ids {
            if self.records[&id].metadata.state == FlowState::Active {
                self.skip_body(id, Side::Request, "Cleared during capture");
                self.skip_body(id, Side::Response, "Cleared during capture");
                let record = self.records.get_mut(&id).unwrap();
                self.counters.memory -=
                    record.message_payload + record.messages.len() * MESSAGE_OVERHEAD;
                record.message_payload = 0;
                record.messages = Vec::new();
                record.messages_cleared = true;
            } else {
                self.remove(id);
            }
        }
    }

    fn make_room(&mut self, needed: usize) -> bool {
        while self.counters.memory.saturating_add(needed) > self.memory_limit {
            let Some((id, side)) = self.bodies.pop_front() else {
                return false;
            };
            self.skip_body(id, side, "memory limit");
            self.counters.evicted_bodies += 1;
        }
        true
    }

    pub fn start(
        &mut self,
        connection: u64,
        process: Attribution,
        kind: FlowKind,
        mut request: RequestHead,
        raw_headers: &HeaderMap,
    ) -> Option<u64> {
        if self.counters.paused {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        if self.records.len() == FLOW_LIMIT {
            self.remove(*self.records.first_key_value()?.0);
            self.counters.flows_dropped += 1;
        }
        let host = request.host.clone();
        let path = request.path.clone();
        let method = request.method.clone();
        let masked = presentation::url(&request.url, Secrets::Mask);
        let private_url =
            (masked != request.url).then(|| std::mem::replace(&mut request.url, masked));
        // The masked URL is held twice: in the request head and in row metadata.
        let fixed = request.url.len() * 2
            + private_url.as_ref().map_or(0, String::len)
            + host.len() * 2
            + path.len() * 2
            + method.len() * 2
            + request.version.len();
        while !self.make_room(fixed) {
            let oldest = *self.records.first_key_value()?.0;
            self.remove(oldest);
            self.counters.flows_dropped += 1;
        }
        let headers = self.capture_headers(raw_headers, fixed, true);
        request.headers = headers.display;
        self.counters.memory += fixed;
        if process != PENDING_ATTRIBUTION && matches!(process, Attribution::Unknown(_)) {
            self.counters.unattributed += 1;
        }
        let classifier = Classifier::new(&path, content_type(raw_headers));
        let content = classifier.kind(kind);
        self.records.insert(
            id,
            Record {
                head_bytes: fixed + headers.bytes,
                metadata: Metadata {
                    id,
                    connection,
                    started: SystemTime::now(),
                    process,
                    kind,
                    repeated: false,
                    method,
                    host,
                    path,
                    status: None,
                    timing: Timing::default(),
                    bytes: (0, 0),
                    state: FlowState::Active,
                    url: request.url.clone(),
                    content,
                    websocket: false,
                },
                request,
                response: None,
                private_url,
                private_headers: [headers.private, Vec::new()],
                bodies: if kind == FlowKind::Tunnel {
                    [
                        Body::Skipped("Not decrypted"),
                        Body::Skipped("Not decrypted"),
                    ]
                } else {
                    Default::default()
                },
                headers_truncated: headers.truncated,
                request_headers_truncated: headers.truncated,
                request_complete: false,
                request_has_trailers: false,
                opaque_request_headers: headers.opaque,
                messages: Vec::new(),
                message_count: 0,
                message_payload: 0,
                message_work: 0,
                messages_cleared: false,
                websocket: false,
                classifier,
                reported: false,
                revision: 0,
            },
        );
        self.changed(id);
        Some(id)
    }

    fn capture_headers(
        &mut self,
        headers: &HeaderMap,
        reserved: usize,
        retain_opaque: bool,
    ) -> CapturedHeaders {
        let mut captured = CapturedHeaders::default();
        let mut wire_bytes = 0;
        for (name, value) in headers {
            if name == http::header::PROXY_AUTHORIZATION {
                continue;
            }
            let len = name.as_str().len() + value.as_bytes().len() + 4;
            let raw = value;
            let value = String::from_utf8_lossy(raw.as_bytes());
            let opaque = retain_opaque && matches!(value, std::borrow::Cow::Owned(_));
            let secret = presentation::secret_header(name.as_str());
            let memory = name.as_str().len()
                + value.len()
                + 4
                + if secret { presentation::MASK.len() } else { 0 }
                + if opaque { raw.as_bytes().len() } else { 0 };
            if wire_bytes + len > HEAD_LIMIT || !self.make_room(reserved + memory) {
                captured.truncated = true;
                break;
            }
            if opaque {
                captured.opaque.push((captured.display.len(), raw.clone()));
            }
            let name = name.to_string();
            if secret {
                captured
                    .private
                    .push((captured.display.len(), value.into_owned()));
                captured.display.push((name, presentation::MASK.into()));
            } else {
                captured.display.push((name, value.into_owned()));
            }
            wire_bytes += len;
            captured.bytes += memory;
            self.counters.memory += memory;
        }
        captured
    }

    pub fn response(&mut self, id: Option<u64>, status: u16, headers: &HeaderMap, ttfb: Duration) {
        let Some(id) = id.filter(|id| self.records.contains_key(id)) else {
            return;
        };
        let content = content_type(headers);
        let headers = if self.counters.paused {
            CapturedHeaders {
                truncated: !headers.is_empty(),
                ..CapturedHeaders::default()
            }
        } else {
            self.capture_headers(headers, 0, false)
        };
        let record = self.records.get_mut(&id).unwrap();
        record.classifier.response(content);
        record.metadata.content = record.classifier.kind(record.metadata.kind);
        record.head_bytes += headers.bytes;
        record.headers_truncated |= headers.truncated;
        record.private_headers[Side::Response as usize] = headers.private;
        record.metadata.status = Some(status);
        record.metadata.timing.ttfb = Some(ttfb);
        record.response = Some(ResponseHead {
            status,
            version: "HTTP/1.1".into(),
            headers: headers.display,
        });
        self.changed(id);
    }

    pub fn response_version(&mut self, id: Option<u64>, version: http::Version) {
        if let Some(head) = id
            .and_then(|id| self.records.get_mut(&id))
            .and_then(|record| record.response.as_mut())
        {
            head.version = format!("{version:?}");
        }
    }

    pub fn waited_for_attribution(&mut self, id: Option<u64>, elapsed: Duration) {
        if let Some(record) = id.and_then(|id| self.records.get_mut(&id)) {
            record.metadata.timing.attribution = Some(elapsed);
            let id = record.metadata.id;
            self.changed(id);
        }
    }

    pub fn discard_active(&mut self, id: Option<u64>) {
        if let Some(id) = id {
            self.remove(id);
        }
    }

    pub fn tls(&mut self, id: Option<u64>, elapsed: Duration) {
        if let Some(record) = id.and_then(|id| self.records.get_mut(&id)) {
            record.metadata.timing.tls = Some(elapsed);
            let id = record.metadata.id;
            self.changed(id);
        }
    }

    pub fn connected(&mut self, id: Option<u64>, elapsed: Duration) {
        if let Some(record) = id.and_then(|id| self.records.get_mut(&id)) {
            record.metadata.timing.connect = Some(elapsed);
            let id = record.metadata.id;
            self.changed(id);
        }
    }

    pub fn count(&mut self, id: Option<u64>, side: Side, bytes: u64) {
        match side {
            Side::Request => self.counters.sent += bytes,
            Side::Response => self.counters.received += bytes,
        }
        if let Some(record) = id.and_then(|id| self.records.get_mut(&id)) {
            let total = match side {
                Side::Request => &mut record.metadata.bytes.0,
                Side::Response => &mut record.metadata.bytes.1,
            };
            *total += bytes;
            if let Body::Truncated {
                total: recorded, ..
            } = &mut record.bodies[side as usize]
            {
                *recorded = *total;
            }
            let id = record.metadata.id;
            self.changed(id);
        }
    }

    pub fn body(&mut self, id: Option<u64>, side: Side, bytes: &[u8]) {
        self.count(id, side, bytes.len() as u64);
        let Some(id) = id else { return };
        let Some(record) = self.records.get(&id) else {
            return;
        };
        if self.counters.paused || bytes.is_empty() {
            return;
        }
        let Body::Captured(kept) = &record.bodies[side as usize] else {
            return;
        };
        let len = kept.len();
        let add = bytes.len().min(BODY_LIMIT - len);
        if !self.make_room(add) {
            self.bodies.retain(|candidate| *candidate != (id, side));
            self.skip_body(id, side, "memory limit");
            self.counters.evicted_bodies += 1;
            return;
        }
        let record = self.records.get_mut(&id).unwrap();
        let body = &mut record.bodies[side as usize];
        let Body::Captured(kept) = body else { return };
        if len == 0 && add > 0 {
            let index = self.bodies.partition_point(|(flow_id, _)| *flow_id <= id);
            self.bodies.insert(index, (id, side));
        }
        if add != 0 {
            // Copy only when recording extends a buffer shared with a detail snapshot.
            if let Some(buffer) = Arc::get_mut(kept) {
                buffer.reserve_exact(add);
                buffer.extend_from_slice(&bytes[..add]);
            } else {
                let mut updated = Vec::with_capacity(len + add);
                updated.extend_from_slice(kept);
                updated.extend_from_slice(&bytes[..add]);
                *kept = Arc::new(updated);
            }
        }
        self.counters.memory += add;
        if add < bytes.len() {
            let total = match side {
                Side::Request => record.metadata.bytes.0,
                Side::Response => record.metadata.bytes.1,
            };
            *body = Body::Truncated {
                kept: std::mem::take(kept),
                total,
            };
        }
    }

    pub fn finish(&mut self, id: Option<u64>, state: FlowState, total: Duration) {
        let Some(id) = id.filter(|id| {
            self.records
                .get(id)
                .is_some_and(|r| r.metadata.state == FlowState::Active)
        }) else {
            return;
        };
        let extra = match &state {
            FlowState::UpstreamTls(reason) => reason.len(),
            _ => 0,
        };
        let (state, reserved) = if self.make_room(extra) {
            (state, extra)
        } else {
            (FlowState::Failed("Upstream TLS failed"), 0)
        };
        if let Some(record) = self.records.get_mut(&id) {
            record.metadata.state = state;
            record.metadata.timing.total = total;
            record.head_bytes += reserved;
            self.counters.memory += reserved;
            self.changed(id);
        }
    }

    pub fn attribute(&mut self, connection: u64, owner: Attribution) {
        let mut changed = Vec::new();
        for record in self.records.values_mut() {
            if record.metadata.connection == connection
                && record.metadata.process == PENDING_ATTRIBUTION
                && owner != PENDING_ATTRIBUTION
            {
                record.metadata.process = owner.clone();
                if matches!(owner, Attribution::Unknown(_)) {
                    self.counters.unattributed += 1;
                }
                changed.push(record.metadata.id);
            }
        }
        for id in changed {
            self.changed(id);
        }
    }

    pub fn updates(&mut self, revision: &mut u64) -> Option<crate::RowUpdates> {
        if *revision == self.revision {
            return None;
        }
        let needs_snapshot = *revision == 0 || self.events_overflowed;
        *revision = self.revision;
        let events = self.drain_events();
        Some(if needs_snapshot {
            crate::RowUpdates::Snapshot(
                self.records
                    .values()
                    .rev()
                    .map(|record| record.metadata.clone())
                    .collect(),
            )
        } else {
            crate::RowUpdates::Changed(
                events
                    .into_iter()
                    .map(|id| {
                        (
                            id,
                            self.records.get(&id).map(|record| record.metadata.clone()),
                        )
                    })
                    .collect(),
            )
        })
    }

    pub fn finished(
        &mut self,
        include_headers: bool,
    ) -> Vec<(Metadata, Option<(Headers, Headers)>)> {
        self.records
            .values_mut()
            .filter_map(|record| {
                if record.reported
                    || record.metadata.state == FlowState::Active
                    || record.metadata.process == PENDING_ATTRIBUTION
                {
                    return None;
                }
                record.reported = true;
                let headers = include_headers.then(|| {
                    (
                        record.request.headers.clone(),
                        record
                            .response
                            .as_ref()
                            .map(|head| head.headers.clone())
                            .unwrap_or_default(),
                    )
                });
                Some((record.metadata.clone(), headers))
            })
            .collect()
    }

    pub fn flow_since(&self, id: u64, reveal: bool, revision: Option<u64>) -> Option<(u64, Flow)> {
        let current = self.records.get(&id)?.revision;
        if revision == Some(current) {
            return None;
        }
        Some((current, self.flow(id, reveal)?))
    }

    pub fn upgraded(&mut self, id: Option<u64>, websocket: bool) {
        let Some(id) = id.filter(|id| self.records.contains_key(id)) else {
            return;
        };
        let record = self.records.get_mut(&id).unwrap();
        record.metadata.kind = FlowKind::Upgrade;
        record.metadata.content = record.classifier.kind(FlowKind::Upgrade);
        record.metadata.websocket = websocket;
        record.websocket = websocket;
        self.bodies.retain(|(owner, _)| *owner != id);
        for side in [Side::Request, Side::Response] {
            let reason = if websocket {
                "Upgraded connection; see Messages for WebSocket traffic"
            } else {
                "Upgraded connection"
            };
            self.skip_body(id, side, reason);
        }
    }

    pub fn reserve_message_payload(&mut self, id: u64, bytes: usize) -> bool {
        let available = !self.counters.paused
            && self.records.get(&id).is_some_and(|record| {
                !record.messages_cleared
                    && record.messages.len() < crate::websocket::FRAME_LIMIT
                    && record.message_payload + record.message_work + bytes
                        <= crate::websocket::PAYLOAD_LIMIT
            });
        if !available || !self.make_room(bytes) {
            return false;
        }
        self.records.get_mut(&id).unwrap().message_work += bytes;
        self.counters.memory += bytes;
        true
    }

    pub fn release_message_payload(&mut self, id: u64, bytes: usize) {
        self.counters.memory -= bytes;
        if let Some(record) = self.records.get_mut(&id) {
            record.message_work -= bytes;
        }
    }

    pub fn message(&mut self, id: u64, message: crate::Message) {
        self.release_message_payload(id, message.payload.len());
        if self.counters.paused {
            return;
        }
        let Some(record) = self.records.get_mut(&id) else {
            return;
        };
        record.message_count += 1;
        let needed = message.payload.len() + MESSAGE_OVERHEAD;
        if !record.messages_cleared
            && record.messages.len() < crate::websocket::FRAME_LIMIT
            && self.make_room(needed)
        {
            let record = self.records.get_mut(&id).unwrap();
            record.message_payload += message.payload.len();
            record.messages.push(Arc::new(message));
            self.counters.memory += needed;
        }
        self.changed(id);
    }

    pub fn request_progress(&mut self, id: Option<u64>, complete: bool, trailers: bool) {
        let Some(record) = id.and_then(|id| self.records.get_mut(&id)) else {
            return;
        };
        let finished = complete && !record.request_complete;
        record.request_complete |= complete;
        record.request_has_trailers |= trailers;
        if !finished {
            return;
        }
        // Classify once, from the complete body, when the request ends.
        if let Body::Captured(body) = &record.bodies[Side::Request as usize] {
            record.classifier.request_body(body);
            let content = record.classifier.kind(record.metadata.kind);
            if content != record.metadata.content {
                record.metadata.content = content;
                let id = record.metadata.id;
                self.changed(id);
            }
        }
    }

    pub fn repeated(&mut self, id: Option<u64>) {
        if let Some(record) = id.and_then(|id| self.records.get_mut(&id)) {
            record.metadata.repeated = true;
        }
    }

    fn replayable(record: &Record) -> bool {
        crate::replay::supported(record.metadata.kind)
            && record.metadata.state != FlowState::Active
            && record.request_complete
            && !record.request_has_trailers
            && !record.request_headers_truncated
            && matches!(record.bodies[0], Body::Captured(_))
    }

    pub fn replay_draft(&self, id: u64, reveal: bool) -> Option<crate::ReplayDraft> {
        let flow = self.flow(id, reveal)?;
        flow.replayable
            .then(|| crate::ReplayDraft::from_flow(flow, reveal))
    }

    pub fn replay_request(
        &self,
        id: u64,
        edits: crate::ReplayEdits,
    ) -> Result<http::Request<bytes::Bytes>, &'static str> {
        use crate::ReplayValue;
        let record = self
            .records
            .get(&id)
            .ok_or("The original flow is no longer available")?;
        if !Self::replayable(record) {
            return Err("Replay requires a complete captured HTTP request");
        }

        let original_url = record.private_url.as_ref().unwrap_or(&record.request.url);
        let url = edits.url.map_or_else(
            || original_url.clone(),
            |url| {
                if edits.preserve_masked_query {
                    crate::replay::restore_query(original_url, url)
                } else {
                    url
                }
            },
        );
        let mut request = http::Request::builder()
            .method(edits.method.as_deref().unwrap_or(&record.request.method))
            .uri(url)
            .body(match edits.body {
                Some(body) => bytes::Bytes::from(body),
                None => match &record.bodies[0] {
                    Body::Captured(body) => bytes::Bytes::copy_from_slice(body),
                    _ => unreachable!("replayable checked above"),
                },
            })
            .map_err(|_| "Enter a valid HTTP method and absolute URL")?;
        let original = |index: usize| {
            if let Ok(position) = record
                .opaque_request_headers
                .binary_search_by_key(&index, |(index, _)| *index)
            {
                return Ok(record.opaque_request_headers[position].1.clone());
            }
            let headers = &record.private_headers[0];
            let value = headers
                .binary_search_by_key(&index, |(index, _)| *index)
                .ok()
                .map(|position| &headers[position].1)
                .or_else(|| record.request.headers.get(index).map(|(_, value)| value))
                .ok_or("An original header is no longer available")?;
            http::HeaderValue::from_str(value).map_err(|_| "Invalid captured header value")
        };
        let headers = edits.headers.unwrap_or_else(|| {
            record
                .request
                .headers
                .iter()
                .enumerate()
                .map(|(index, (name, _))| crate::ReplayHeader {
                    name: name.clone(),
                    value: ReplayValue::Original(index),
                })
                .collect()
        });
        for header in headers {
            let name =
                http::HeaderName::try_from(header.name).map_err(|_| "Enter a valid header name")?;
            if name == http::header::PROXY_AUTHORIZATION {
                continue;
            }
            let value = match header.value {
                ReplayValue::Original(index) => original(index)?,
                ReplayValue::Edited(value) => {
                    http::HeaderValue::from_str(&value).map_err(|_| "Enter a valid header value")?
                }
            };
            request.headers_mut().append(name, value);
        }
        // Hyper determines the framing from the retained or edited body.
        request.headers_mut().remove(http::header::CONTENT_LENGTH);
        Ok(request)
    }

    pub fn flow(&self, id: u64, reveal: bool) -> Option<Flow> {
        let record = self.records.get(&id)?;
        let mut request = record.request.clone();
        let mut response = record.response.clone();
        if reveal {
            if let Some(url) = &record.private_url {
                request.url.clone_from(url);
            }
            for (index, value) in &record.private_headers[Side::Request as usize] {
                request.headers[*index].1.clone_from(value);
            }
            if let Some(head) = &mut response {
                for (index, value) in &record.private_headers[Side::Response as usize] {
                    head.headers[*index].1.clone_from(value);
                }
            }
        }
        let auth_scheme = record
            .request
            .headers
            .iter()
            .position(|(name, _)| name == "authorization")
            .and_then(|index| {
                let value = record.private_headers[Side::Request as usize]
                    .iter()
                    .find(|(position, _)| *position == index)
                    .map_or(&record.request.headers[index].1, |(_, value)| value);
                // A lone token is the credential itself, never a scheme.
                let (scheme, credentials) = value.trim().split_once(char::is_whitespace)?;
                (!credentials.trim().is_empty()
                    && scheme.len() <= 32
                    && scheme
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)))
                .then(|| scheme.to_owned())
            });
        Some(Flow {
            metadata: record.metadata.clone(),
            request,
            response,
            request_body: record.bodies[0].clone(),
            response_body: record.bodies[1].clone(),
            headers_truncated: record.headers_truncated,
            replayable: Self::replayable(record),
            messages: record.messages.clone(),
            message_count: record.message_count,
            websocket: record.websocket,
            auth_scheme,
        })
    }
}

fn content_type(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(store: &mut Store) -> u64 {
        store
            .start(
                0,
                Attribution::Unknown("test"),
                FlowKind::Http,
                RequestHead {
                    method: "GET".into(),
                    url: "http://x/".into(),
                    host: "x:80".into(),
                    path: "/".into(),
                    version: "HTTP/1.1".into(),
                    headers: Vec::new(),
                },
                &HeaderMap::new(),
            )
            .unwrap()
    }

    #[test]
    fn selected_snapshots_share_bodies_and_require_a_new_revision() {
        let mut store = Store::default();
        let selected = start(&mut store);
        let unrelated = start(&mut store);
        store.body(Some(selected), Side::Response, &[1; 128]);
        let (revision, original) = store.flow_since(selected, false, None).unwrap();
        let other_view = store.flow(selected, true).unwrap();
        assert_eq!(
            original.response_body.kept().as_ptr(),
            other_view.response_body.kept().as_ptr()
        );

        store.body(Some(unrelated), Side::Request, &[2; 32]);
        assert!(store.flow_since(selected, false, Some(revision)).is_none());

        store.body(Some(selected), Side::Response, &[3; 32]);
        let (appended, updated) = store.flow_since(selected, false, Some(revision)).unwrap();
        assert_eq!(original.response_body.kept(), &[1; 128]);
        assert_eq!(&updated.response_body.kept()[128..], &[3; 32]);

        store.pause(true);
        let (paused, updated) = store.flow_since(selected, false, Some(appended)).unwrap();
        assert!(matches!(
            updated.response_body,
            Body::Truncated { total: 160, .. }
        ));
        store.clear();
        let (_, updated) = store.flow_since(selected, false, Some(paused)).unwrap();
        assert!(matches!(
            updated.response_body,
            Body::Skipped("Cleared during capture")
        ));
    }

    #[test]
    fn counts_only_final_unknown_attributions_once_per_flow() {
        let mut store = Store::default();
        let pending_request = |store: &mut Store, connection| {
            let request = store.flow(0, false).unwrap().request;
            store
                .start(
                    connection,
                    PENDING_ATTRIBUTION,
                    FlowKind::Http,
                    request,
                    &HeaderMap::new(),
                )
                .unwrap()
        };
        let already_unknown = start(&mut store);
        store.attribute(0, Attribution::Unknown("duplicate"));
        assert_eq!(store.counters.unattributed, 1);
        assert_eq!(
            store.flow(already_unknown, false).unwrap().metadata.process,
            Attribution::Unknown("test")
        );
        let pending_unknown = pending_request(&mut store, 1);
        let pending_known = pending_request(&mut store, 2);
        assert_eq!(store.counters.unattributed, 1);
        store.finish(Some(pending_unknown), FlowState::Complete, Duration::ZERO);
        assert!(store.finished(false).is_empty());

        store.attribute(1, Attribution::Unknown("final"));
        store.attribute(1, Attribution::Unknown("duplicate"));
        let known = Attribution::Known {
            name: "test".into(),
            pid: 9,
            start_time: 1,
            unique_id: 2,
        };
        store.attribute(2, known.clone());
        assert_eq!(store.counters.unattributed, 2);
        assert_eq!(store.finished(false).len(), 1);
        assert_eq!(
            store.flow(pending_known, false).unwrap().metadata.process,
            known
        );
    }

    #[test]
    fn charges_active_bodies_and_evicts_oldest_first() {
        let mut store = Store {
            memory_limit: 155,
            ..Store::default()
        };
        let first = start(&mut store);
        let second = start(&mut store);
        let heads = store.counters.memory;
        store.body(Some(first), Side::Response, &[0; 40]);
        assert_eq!(store.counters.memory, heads + 40);
        store.body(Some(second), Side::Response, &[1; 40]);
        store.body(Some(second), Side::Response, &[1; 20]);
        assert!(matches!(
            store.flow(first, false).unwrap().response_body,
            Body::Skipped("memory limit")
        ));
        assert_eq!(
            store.flow(second, false).unwrap().response_body.kept(),
            &[1; 60]
        );
        assert_eq!(store.counters.memory, heads + 60);
        assert_eq!(store.counters.evicted_bodies, 1);

        store.clear();
        assert_eq!(store.counters.memory, heads);
        store.body(Some(second), Side::Response, &[1; 10]);
        assert_eq!(store.counters.memory, heads);
        assert_eq!(store.flow(second, false).unwrap().metadata.bytes.1, 70);
    }

    #[test]
    fn enforces_header_flow_and_event_limits() {
        let mut store = Store::default();
        let first = start(&mut store);
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-large",
            http::HeaderValue::from_bytes(&vec![b'a'; HEAD_LIMIT]).unwrap(),
        );
        store.response(Some(first), 200, &headers, Duration::ZERO);
        let flow = store.flow(first, false).unwrap();
        assert!(flow.headers_truncated);
        assert!(flow.response.unwrap().headers.is_empty());

        let mut revision = 0;
        store.updates(&mut revision);
        for _ in 0..(FLOW_LIMIT - 1) {
            start(&mut store);
        }
        assert_eq!(store.counters.flows_dropped, 0);
        assert!(
            matches!(store.updates(&mut revision), Some(crate::RowUpdates::Snapshot(rows)) if rows.len() == FLOW_LIMIT)
        );
        start(&mut store);
        assert!(store.flow(first, false).is_none());
        assert_eq!(store.records.len(), FLOW_LIMIT);
        assert_eq!(store.counters.flows_dropped, 1);
        assert_eq!(store.drain_events().len(), 2);
    }

    fn start_with(
        store: &mut Store,
        kind: FlowKind,
        url: &str,
        path: &str,
        headers: &[(&'static str, &str)],
    ) -> u64 {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.append(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_str(value).unwrap(),
            );
        }
        store
            .start(
                0,
                Attribution::Unknown("test"),
                kind,
                RequestHead {
                    method: "POST".into(),
                    url: url.into(),
                    host: "x:80".into(),
                    path: path.into(),
                    version: "HTTP/1.1".into(),
                    headers: Vec::new(),
                },
                &map,
            )
            .unwrap()
    }

    #[test]
    fn classifies_content_once_per_flow_event() {
        use crate::ContentKind;
        let mut store = Store::default();
        let graphql = start_with(
            &mut store,
            FlowKind::Http,
            "http://x/api",
            "/api",
            &[("content-type", "application/json")],
        );
        let row = |store: &Store, id| store.flow(id, false).unwrap().metadata;
        assert_eq!(row(&store, graphql).content, ContentKind::Json);
        store.body(Some(graphql), Side::Request, br#"{"query":"{ me }"}"#);
        store.request_progress(Some(graphql), true, false);
        assert_eq!(row(&store, graphql).content, ContentKind::GraphQl);
        let mut json = HeaderMap::new();
        json.insert(
            http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        store.response(Some(graphql), 200, &json, Duration::ZERO);
        assert_eq!(row(&store, graphql).content, ContentKind::GraphQl);

        let page = start_with(&mut store, FlowKind::Http, "http://x/", "/", &[]);
        let mut html = HeaderMap::new();
        html.insert(http::header::CONTENT_TYPE, "text/html".parse().unwrap());
        store.response(Some(page), 200, &html, Duration::ZERO);
        assert_eq!(row(&store, page).content, ContentKind::Document);

        let tunnel = start_with(&mut store, FlowKind::Tunnel, "x:443", "", &[]);
        assert_eq!(row(&store, tunnel).content, ContentKind::Other);

        let socket = start_with(&mut store, FlowKind::Http, "http://x/ws", "/ws", &[]);
        store.upgraded(Some(socket), true);
        let metadata = row(&store, socket);
        assert!(metadata.websocket);
        assert_eq!(metadata.content, ContentKind::Other);
    }

    #[test]
    fn rows_carry_masked_urls_and_details_expose_only_the_auth_scheme() {
        let mut store = Store::default();
        let before = store.counters.memory;
        let id = start_with(
            &mut store,
            FlowKind::Http,
            "http://x/a?token=secret&page=2",
            "/a",
            &[("authorization", "Bearer secret-value")],
        );
        let flow = store.flow(id, false).unwrap();
        assert_eq!(flow.metadata.url, "http://x/a?token=[redacted]&page=2");
        assert_eq!(flow.auth_scheme.as_deref(), Some("Bearer"));
        assert!(store.counters.memory - before >= flow.metadata.url.len() * 2);
        let lone = start_with(
            &mut store,
            FlowKind::Http,
            "http://x/",
            "/",
            &[("authorization", "secret-value")],
        );
        assert_eq!(store.flow(lone, false).unwrap().auth_scheme, None);
        assert_eq!(store.flow(lone, true).unwrap().auth_scheme, None);
    }
}
