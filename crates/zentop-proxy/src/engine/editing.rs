//! Runs live-editing rules on plaintext requests: Map Local or Map Remote,
//! then rewrites, then breakpoints, on the request and then the response.
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use bytes::Bytes;
use http::header::{self, HeaderName, HeaderValue};
use http::{request, response, Request, Response, StatusCode, Uri, Version};
use http_body_util::{BodyExt, Full};
use tokio_rustls::rustls;

use super::{reply, Capture, Connection, Engine, MessageBody, StreamingBody, Target};
use crate::breakpoints::{HeldEdits, Outcome, Pending, Queue};
use crate::rules::{self, Action, BodyEdit, Compiled, RuleSet, RuleSide};
use crate::store::Side;
use crate::{Attribution, FlowState, ReplayHeader, ReplayValue};

/// Rules and held messages shared by the session and its engine.
pub(crate) struct Editing {
    rules: Mutex<Arc<RuleSet>>,
    pub queue: Queue,
    upstream: Option<Arc<rustls::ClientConfig>>,
    platform: OnceLock<Option<Arc<rustls::ClientConfig>>>,
}

impl Editing {
    pub fn new(upstream: Option<Arc<rustls::ClientConfig>>) -> Self {
        Self {
            rules: Mutex::default(),
            queue: Queue::default(),
            upstream,
            platform: OnceLock::new(),
        }
    }

    pub fn rules(&self) -> Arc<RuleSet> {
        self.rules.lock().unwrap().clone()
    }

    pub fn set_rules(&self, rules: RuleSet) {
        let breakpoints = rules.breakpoints_enabled();
        *self.rules.lock().unwrap() = Arc::new(rules);
        if !breakpoints {
            self.queue
                .release_all("released when breakpoints were turned off");
        }
    }

    /// The verified upstream TLS configuration: the session's (which tests
    /// can inject), or the platform verifier.
    fn upstream(&self) -> Option<Arc<rustls::ClientConfig>> {
        self.upstream.clone().or_else(|| {
            self.platform
                .get_or_init(|| {
                    crate::Decryption::platform()
                        .ok()
                        .map(|decryption| decryption.upstream)
                })
                .clone()
        })
    }

    fn holding(&self, capture: &Capture) -> bool {
        self.rules().breakpoints_enabled() && !capture.store.lock().unwrap().counters.paused
    }

    /// Holds a message until it is executed, aborted, released or times out.
    async fn hold(&self, rule: &Compiled, capture: &Capture, message: Held<'_>) -> Decision {
        let raw: Vec<(String, String)> = message
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let (body, body_note, reserved) = match message.body {
            Some(bytes) if capture.store.lock().unwrap().reserve_hold(bytes.len()) => {
                let len = bytes.len();
                (Some(bytes), None, len)
            }
            Some(_) => (
                None,
                Some("Body not editable: the capture memory budget is full"),
                0,
            ),
            _ => (None, Some("Body over 1 MB is sent unchanged"), 0),
        };
        let release = HeldMemory {
            capture,
            bytes: reserved,
        };
        let timeout = self.rules().hold_timeout;
        let Some((id, mut outcome)) = self.queue.hold(Pending {
            flow: capture.id,
            rule: rule.rule.name.clone(),
            response: message.response,
            method: message.method,
            url: message.url,
            status: message.status,
            headers: raw,
            body,
            body_note,
            timeout,
        }) else {
            capture.store.lock().unwrap().hold_skipped();
            return Decision::Continue(format!(
                "Passed through: {} messages were already held",
                rules::HOLD_LIMIT
            ));
        };
        // A connection that closes while held leaves the queue with its task.
        let _cancel = Cancel(&self.queue, id);
        let started = Instant::now();
        let result = tokio::select! {
            result = &mut outcome => result.ok(),
            () = tokio::time::sleep(timeout) => None,
        };
        let held = format!("Held {:.1} s", started.elapsed().as_secs_f64());
        let result = match result {
            Some(result) => Some(result),
            None if self.queue.cancel(id) => None,
            // Resolved as the timer fired; the outcome is already on its way.
            None => outcome.await.ok(),
        };
        drop(release);
        match result {
            None => {
                capture.store.lock().unwrap().hold_timed_out();
                Decision::Continue(format!("{held}, timed out; continued unmodified"))
            }
            Some(Outcome::Execute(edits)) => Decision::Execute(edits, held),
            Some(Outcome::Abort) => Decision::Abort(format!("{held}, aborted")),
            Some(Outcome::Release(reason)) => Decision::Continue(format!("{held}, {reason}")),
        }
    }
}

struct Cancel<'a>(&'a Queue, u64);
impl Drop for Cancel<'_> {
    fn drop(&mut self) {
        self.0.cancel(self.1);
    }
}

struct HeldMemory<'a> {
    capture: &'a Capture,
    bytes: usize,
}
impl Drop for HeldMemory<'_> {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.capture.store.lock().unwrap().release_hold(self.bytes);
        }
    }
}

struct Held<'a> {
    response: bool,
    method: String,
    url: String,
    status: Option<u16>,
    headers: &'a http::HeaderMap,
    /// The body, when it was read completely.
    body: Option<Bytes>,
}

enum Decision {
    Execute(HeldEdits, String),
    Abort(String),
    Continue(String),
}

/// A message body that rules may read before forwarding.
pub(super) enum Payload {
    Stream(MessageBody),
    Complete(Bytes),
    /// Over the edit limit: the bytes already read, then the rest.
    Partial(Bytes, MessageBody),
}

impl Payload {
    /// Reads up to [`rules::EDIT_BODY_LIMIT`] bytes. A declared length over
    /// the limit is not read at all.
    async fn read(self, headers: &http::HeaderMap) -> Result<Self, hyper::Error> {
        let Self::Stream(mut body) = self else {
            return Ok(self);
        };
        let limit = rules::EDIT_BODY_LIMIT;
        let declared = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if declared.is_some_and(|length| length > limit as u64) {
            return Ok(Self::Partial(Bytes::new(), body));
        }
        let mut buffer = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame?.into_data() {
                buffer.extend_from_slice(&data);
                if buffer.len() > limit {
                    return Ok(Self::Partial(buffer.into(), body));
                }
            }
        }
        Ok(Self::Complete(buffer.into()))
    }

    fn complete(&self) -> Option<Bytes> {
        match self {
            Self::Complete(bytes) => Some(bytes.clone()),
            _ => None,
        }
    }

    fn into_body(self) -> MessageBody {
        match self {
            Self::Stream(body) => body,
            Self::Complete(bytes) => full(bytes),
            Self::Partial(prefix, rest) => Prefixed {
                prefix: (!prefix.is_empty()).then_some(prefix),
                rest,
            }
            .boxed_unsync(),
        }
    }
}

fn full(bytes: Bytes) -> MessageBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// Bytes read for a rule, followed by the rest of the original body.
struct Prefixed {
    prefix: Option<Bytes>,
    rest: MessageBody,
}

impl hyper::body::Body for Prefixed {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, hyper::Error>>> {
        if let Some(prefix) = self.prefix.take() {
            return std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(prefix))));
        }
        std::pin::Pin::new(&mut self.rest).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.prefix.is_none() && self.rest.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        let mut hint = self.rest.size_hint();
        let prefix = self.prefix.as_ref().map_or(0, |prefix| prefix.len() as u64);
        hint.set_lower(hint.lower() + prefix);
        if let Some(upper) = hint.upper() {
            hint.set_upper(upper + prefix);
        }
        hint
    }
}

/// Response rules carried into the forwarding path.
pub(super) struct ResponseStage {
    editing: Arc<Editing>,
    rewrites: Vec<Arc<Compiled>>,
    breakpoint: Option<Arc<Compiled>>,
    method: String,
    url: String,
}

impl ResponseStage {
    /// Applies response rewrites and the response breakpoint. An abort
    /// returns the `502` to send instead.
    pub(super) async fn apply(
        self,
        head: &mut response::Parts,
        body: MessageBody,
        capture: &Capture,
    ) -> Result<MessageBody, Box<Response<MessageBody>>> {
        let mut payload = Payload::Stream(body);
        let upstream_failed = |capture: &Capture| {
            let reason = "Upstream HTTP request failed";
            capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
            reply(StatusCode::BAD_GATEWAY, reason)
        };
        for rule in &self.rewrites {
            let mut notes = Vec::new();
            if rules::rewrite_status(rule, &mut head.status) {
                notes.push(format!("Status set to {}", head.status.as_u16()));
            }
            let applied = rules::rewrite_headers(rule, RuleSide::Response, &mut head.headers);
            if applied != 0 {
                notes.push(header_note(applied, "response"));
            }
            if rule.edits_body(RuleSide::Response) {
                payload = match payload.read(&head.headers).await {
                    Ok(payload) => payload,
                    Err(_) => return Err(Box::new(upstream_failed(capture))),
                };
                notes.extend(rewrite_payload(
                    rule,
                    RuleSide::Response,
                    &mut head.headers,
                    &mut payload,
                ));
            }
            mark(capture, rule, notes, "No response change");
        }
        let Some(rule) = self
            .breakpoint
            .as_ref()
            .filter(|_| self.editing.holding(capture))
        else {
            return Ok(payload.into_body());
        };
        payload = match payload.read(&head.headers).await {
            Ok(payload) => payload,
            Err(_) => return Err(Box::new(upstream_failed(capture))),
        };
        let decision = self
            .editing
            .hold(
                rule,
                capture,
                Held {
                    response: true,
                    method: self.method,
                    url: self.url,
                    status: Some(head.status.as_u16()),
                    headers: &head.headers,
                    body: payload.complete(),
                },
            )
            .await;
        match decision {
            Decision::Abort(note) => {
                mark(capture, rule, vec![note], "");
                let reason = "Response aborted at a breakpoint";
                capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
                return Err(Box::new(reply(
                    StatusCode::BAD_GATEWAY,
                    "Zentop breakpoint aborted this response",
                )));
            }
            Decision::Continue(note) => mark(capture, rule, vec![note], ""),
            Decision::Execute(edits, held) => {
                let mut edited = false;
                if let Some(status) = edits
                    .status
                    .and_then(|status| StatusCode::from_u16(status).ok())
                {
                    edited |= status != head.status;
                    head.status = status;
                }
                let note = match apply_edits(&mut head.headers, &mut payload, &edits) {
                    Ok(changed) => {
                        edited |= changed;
                        format!(
                            "{held}, executed{}",
                            if edited { " with edits" } else { "" }
                        )
                    }
                    Err(reason) => format!("{held}, executed unmodified: {reason}"),
                };
                mark(capture, rule, vec![note], "");
            }
        }
        Ok(payload.into_body())
    }
}

fn header_note(count: usize, side: &str) -> String {
    if count == 1 {
        format!("1 {side} header changed")
    } else {
        format!("{count} {side} headers changed")
    }
}

/// Rewrites a read body in place and returns what to note.
fn rewrite_payload(
    rule: &Compiled,
    side: RuleSide,
    headers: &mut http::HeaderMap,
    payload: &mut Payload,
) -> Option<String> {
    let Payload::Complete(bytes) = payload else {
        return Some("Body skipped: over 1 MB".into());
    };
    match rules::rewrite_body(rule, side, headers, bytes) {
        BodyEdit::Unchanged => None,
        BodyEdit::Skipped(reason) => Some(reason.into()),
        BodyEdit::Changed(body) => {
            rules::reframe(headers);
            *payload = Payload::Complete(body.into());
            Some(match side {
                RuleSide::Request => "Request body rewritten".into(),
                RuleSide::Response => "Response body rewritten".into(),
            })
        }
    }
}

fn mark(capture: &Capture, rule: &Compiled, notes: Vec<String>, fallback: &str) {
    let note = if notes.is_empty() {
        fallback.to_owned()
    } else {
        notes.join(", ")
    };
    capture
        .store
        .lock()
        .unwrap()
        .mark(capture.id, &rule.rule, note);
}

/// Applies held header and body edits. Framing headers keep their original
/// values unless the body is replaced. Returns whether anything changed.
fn apply_edits(
    headers: &mut http::HeaderMap,
    payload: &mut Payload,
    edits: &HeldEdits,
) -> Result<bool, &'static str> {
    let mut changed = false;
    let original: Vec<(HeaderName, HeaderValue)> = headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut next = headers.clone();
    if let Some(rows) = &edits.headers {
        next = edited_headers(&original, rows)?;
        for name in [header::CONTENT_LENGTH, header::TRANSFER_ENCODING] {
            next.remove(&name);
            for value in headers.get_all(&name) {
                next.append(name.clone(), value.clone());
            }
        }
        changed |= next != *headers;
    }
    if let Some(body) = &edits.body {
        let replaced = match payload {
            Payload::Complete(bytes) => bytes.as_ref() != body.as_bytes(),
            _ => return Err("the body is too large to edit"),
        };
        if replaced {
            rules::reframe(&mut next);
            next.remove(header::CONTENT_ENCODING);
            *payload = Payload::Complete(Bytes::from(body.clone()));
            changed = true;
        }
    }
    *headers = next;
    Ok(changed)
}

/// Builds headers from editor rows; untouched rows keep original values.
pub(super) fn edited_headers(
    original: &[(HeaderName, HeaderValue)],
    rows: &[ReplayHeader],
) -> Result<http::HeaderMap, &'static str> {
    let mut headers = http::HeaderMap::new();
    for row in rows {
        if row.name.trim().is_empty() {
            continue;
        }
        let name = HeaderName::from_bytes(row.name.trim().as_bytes())
            .map_err(|_| "Enter a valid header name")?;
        if name == header::PROXY_AUTHORIZATION {
            continue;
        }
        let value = match &row.value {
            ReplayValue::Original(index) => original
                .get(*index)
                .map(|(_, value)| value.clone())
                .ok_or("An original header is no longer available")?,
            ReplayValue::Edited(value) => {
                HeaderValue::from_str(value).map_err(|_| "Enter a valid header value")?
            }
        };
        headers.append(name, value);
    }
    Ok(headers)
}

/// Checks edits before they reach a held message.
pub(super) fn validate_edits(edits: &HeldEdits, header_count: usize) -> Result<(), &'static str> {
    if let Some(method) = &edits.method {
        http::Method::from_bytes(method.trim().as_bytes())
            .map_err(|_| "Enter a valid HTTP method")?;
    }
    if let Some(url) = &edits.url {
        let uri: Uri = url
            .trim()
            .parse()
            .map_err(|_| "Enter an absolute http:// or https:// URL")?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
            return Err("Enter an absolute http:// or https:// URL");
        }
    }
    if edits
        .status
        .is_some_and(|status| !(200..=599).contains(&status))
    {
        return Err("Enter a status code from 200 to 599");
    }
    if let Some(rows) = &edits.headers {
        let original: Vec<_> = (0..header_count)
            .map(|_| (HeaderName::from_static("x"), HeaderValue::from_static("")))
            .collect();
        edited_headers(&original, rows)?;
    }
    if edits
        .body
        .as_ref()
        .is_some_and(|body| body.len() > rules::EDIT_BODY_LIMIT)
    {
        return Err("Edited bodies are limited to 1 MB");
    }
    Ok(())
}

/// Points a request at an absolute URL and returns its new target.
fn retarget(parts: &mut request::Parts, url: &str) -> Result<(Target, bool), &'static str> {
    let uri: Uri = url
        .trim()
        .parse()
        .map_err(|_| "The new destination is not a valid URL")?;
    let https = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => return Err("The new destination must use http or https"),
    };
    let probe = Request::builder()
        .method(parts.method.clone())
        .uri(uri.clone())
        .body(())
        .map_err(|_| "The new destination is not a valid URL")?;
    let target = Target::parse(&probe, https)?;
    parts.uri = uri;
    Ok((target, https))
}

fn current_url(https: bool, target: &Target, uri: &Uri) -> String {
    rules::rule_url(
        if https { "https" } else { "http" },
        &target.host,
        target.port,
        uri.path_and_query().map_or("/", |path| path.as_str()),
    )
}

impl Engine {
    /// Routes a plaintext request through the rules, then upstream.
    pub(super) async fn exchange(
        &self,
        request: Request<MessageBody>,
        mut target: Target,
        connection: Arc<Connection>,
        capture: Arc<Capture>,
        mut tls: Option<Arc<rustls::ClientConfig>>,
    ) -> Response<MessageBody> {
        let rules = self.editing.rules();
        if target.tunnel || rules.is_empty() || super::has_upgrade(request.headers()) {
            return self
                .route(request, target, Some(connection), capture, tls, None)
                .await;
        }
        let url = current_url(tls.is_some(), &target, request.uri());
        let app = match &*connection.owner.borrow() {
            Attribution::Known { name, .. } => Some(name.clone()),
            Attribution::Unknown(_) => None,
        };
        let plan = rules.plan(request.method().as_str(), &url, app.as_deref());
        if plan.is_empty() {
            return self
                .route(request, target, Some(connection), capture, tls, None)
                .await;
        }
        let version = request.version();
        let (mut parts, body) = request.into_parts();
        let mut payload = Payload::Stream(body);
        let mut changed = false;
        let body_failed = |capture: &Capture| {
            let reason = "HTTP body transfer failed";
            capture.fail(StatusCode::BAD_REQUEST, FlowState::Failed(reason));
            reply(StatusCode::BAD_REQUEST, reason)
        };
        let stage = |method: &http::Method, url: String| {
            let rewrites: Vec<_> = plan
                .rewrites
                .iter()
                .filter(|rule| rule.touches(RuleSide::Response))
                .cloned()
                .collect();
            (!rewrites.is_empty() || plan.response_break.is_some()).then(|| ResponseStage {
                editing: self.editing.clone(),
                rewrites,
                breakpoint: plan.response_break.clone(),
                method: method.to_string(),
                url,
            })
        };

        // 1. Map Local or Map Remote decides where the request goes.
        if let Some(mapping) = &plan.mapping {
            match &mapping.rule.action {
                Action::MapLocal { path, .. } => {
                    let stage = stage(&parts.method, url);
                    return self
                        .map_local(mapping, path.clone(), payload, capture, version, stage)
                        .await;
                }
                Action::MapRemote { .. } => {
                    let path = parts.uri.path_and_query().map_or("/", |p| p.as_str());
                    let scheme = if tls.is_some() { "https" } else { "http" };
                    let mapped =
                        rules::remote_url(&mapping.rule, scheme, &target.host, target.port, path)
                            .ok_or("The new destination is not a valid URL")
                            .and_then(|url| retarget(&mut parts, &url));
                    let upstream = match mapped {
                        Ok((next, https)) => {
                            target = next;
                            if https {
                                self.editing
                                    .upstream()
                                    .map(Some)
                                    .ok_or("Platform TLS verification is unavailable")
                            } else {
                                Ok(None)
                            }
                        }
                        Err(reason) => Err(reason),
                    };
                    match upstream {
                        Ok(config) => {
                            let https = config.is_some();
                            tls = config;
                            mark(
                                &capture,
                                mapping,
                                vec![format!(
                                    "Sent to {}://{}",
                                    if https { "https" } else { "http" },
                                    target.authority
                                )],
                                "",
                            );
                            changed = true;
                        }
                        Err(reason) => {
                            mark(&capture, mapping, vec![reason.into()], "");
                            capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
                            return reply(
                                StatusCode::BAD_GATEWAY,
                                format!("Zentop Map Remote: {reason}"),
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        // 2. Request rewrites, in order.
        for rule in plan
            .rewrites
            .iter()
            .filter(|rule| rule.touches(RuleSide::Request))
        {
            let mut notes = Vec::new();
            let applied = rules::rewrite_headers(rule, RuleSide::Request, &mut parts.headers);
            if applied != 0 {
                notes.push(header_note(applied, "request"));
            }
            if let Some(uri) = rules::rewrite_query(rule, &parts.uri) {
                parts.uri = uri;
                notes.push("Query changed".into());
            }
            if rule.edits_body(RuleSide::Request) {
                payload = match payload.read(&parts.headers).await {
                    Ok(payload) => payload,
                    Err(_) => return body_failed(&capture),
                };
                notes.extend(rewrite_payload(
                    rule,
                    RuleSide::Request,
                    &mut parts.headers,
                    &mut payload,
                ));
            }
            changed |= !notes.is_empty();
            mark(&capture, rule, notes, "No request change");
        }

        // 3. The request breakpoint.
        if let Some(rule) = plan
            .request_break
            .as_ref()
            .filter(|_| self.editing.holding(&capture))
        {
            payload = match payload.read(&parts.headers).await {
                Ok(payload) => payload,
                Err(_) => return body_failed(&capture),
            };
            let held_url = current_url(tls.is_some(), &target, &parts.uri);
            let decision = self
                .editing
                .hold(
                    rule,
                    &capture,
                    Held {
                        response: false,
                        method: parts.method.to_string(),
                        url: held_url.clone(),
                        status: None,
                        headers: &parts.headers,
                        body: payload.complete(),
                    },
                )
                .await;
            match decision {
                Decision::Abort(note) => {
                    mark(&capture, rule, vec![note], "");
                    let reason = "Request aborted at a breakpoint";
                    capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
                    return reply(
                        StatusCode::BAD_GATEWAY,
                        "Zentop breakpoint aborted this request",
                    );
                }
                Decision::Continue(note) => mark(&capture, rule, vec![note], ""),
                Decision::Execute(edits, held) => {
                    let note = match self.apply_request_edits(
                        &mut parts,
                        &mut payload,
                        &mut target,
                        &mut tls,
                        &held_url,
                        &edits,
                    ) {
                        Ok(true) => {
                            changed = true;
                            format!("{held}, executed with edits")
                        }
                        Ok(false) => format!("{held}, executed"),
                        Err(reason) => format!("{held}, executed unmodified: {reason}"),
                    };
                    mark(&capture, rule, vec![note], "");
                }
            }
        }

        let https = tls.is_some();
        let sent_url = current_url(https, &target, &parts.uri);
        if changed {
            self.store.lock().unwrap().update_request(
                capture.id,
                parts.method.as_str(),
                sent_url.clone(),
                target.authority.clone(),
                parts.uri.path().to_owned(),
                &parts.headers,
            );
        }
        let stage = stage(&parts.method, sent_url);
        let request = Request::from_parts(parts, payload.into_body());
        self.route(request, target, Some(connection), capture, tls, stage)
            .await
    }

    /// Applies breakpoint edits to a request, including a new destination.
    fn apply_request_edits(
        &self,
        parts: &mut request::Parts,
        payload: &mut Payload,
        target: &mut Target,
        tls: &mut Option<Arc<rustls::ClientConfig>>,
        held_url: &str,
        edits: &HeldEdits,
    ) -> Result<bool, &'static str> {
        let mut changed = false;
        if let Some(method) = &edits.method {
            let method = http::Method::from_bytes(method.trim().as_bytes())
                .map_err(|_| "Enter a valid HTTP method")?;
            changed |= method != parts.method;
            parts.method = method;
        }
        if let Some(url) = &edits.url {
            let url = if edits.preserve_masked_query {
                crate::replay::restore_query(held_url, url.trim().to_owned())
            } else {
                url.trim().to_owned()
            };
            if url != held_url {
                let mut next = parts.clone_head();
                let (destination, https) = retarget(&mut next, &url)?;
                let config = if https {
                    Some(
                        self.editing
                            .upstream()
                            .ok_or("Platform TLS verification is unavailable")?,
                    )
                } else {
                    None
                };
                parts.uri = next.uri;
                *target = destination;
                *tls = config;
                changed = true;
            }
        }
        changed |= apply_edits(&mut parts.headers, payload, edits)?;
        Ok(changed)
    }

    /// Answers from a local file, then runs response rules on it.
    async fn map_local(
        &self,
        rule: &Compiled,
        path: String,
        payload: Payload,
        capture: Arc<Capture>,
        version: Version,
        stage: Option<ResponseStage>,
    ) -> Response<MessageBody> {
        // Record the request body as if it had been sent.
        let mut drain = StreamingBody {
            incoming: payload.into_body(),
            side: Side::Request,
            capture: capture.clone(),
            _driver: None,
        };
        while let Some(frame) = drain.frame().await {
            if frame.is_err() {
                break;
            }
        }
        let read =
            tokio::task::spawn_blocking(move || rules::read_local(&path, rules::MAP_LOCAL_LIMIT))
                .await
                .unwrap_or(Err("Map Local file could not be read"));
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(reason) => {
                mark(&capture, rule, vec![reason.into()], "");
                capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
                return reply(
                    StatusCode::BAD_GATEWAY,
                    format!("Zentop Map Local: {reason}"),
                );
            }
        };
        let Some(head) = rules::local_head(&rule.rule, bytes.len()) else {
            let reason = "Map Local response could not be built";
            capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
            return reply(StatusCode::BAD_GATEWAY, reason);
        };
        mark(
            &capture,
            rule,
            vec![format!(
                "Served {} bytes from {}",
                bytes.len(),
                std::path::Path::new(&rule_path(rule))
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
            )],
            "",
        );
        super::deliver(head, full(bytes.into()), capture, None, version, stage).await
    }
}

fn rule_path(rule: &Compiled) -> String {
    match &rule.rule.action {
        Action::MapLocal { path, .. } => path.clone(),
        _ => String::new(),
    }
}

trait CloneHead {
    fn clone_head(&self) -> request::Parts;
}

impl CloneHead for request::Parts {
    fn clone_head(&self) -> request::Parts {
        let mut request = Request::new(());
        *request.method_mut() = self.method.clone();
        *request.uri_mut() = self.uri.clone();
        *request.version_mut() = self.version;
        *request.headers_mut() = self.headers.clone();
        request.into_parts().0
    }
}
