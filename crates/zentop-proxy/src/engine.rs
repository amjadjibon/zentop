use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener as StdListener};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use base64::{prelude::*, Engine as _};
use bytes::Bytes;
use http::header::{self, HeaderValue};
use http::uri::Authority;
use http::{HeaderMap, Method, Request, Response, StatusCode, Version};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body as HttpBody, Frame, Incoming, SizeHint};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{lookup_host, TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::{self, Instant as TokioInstant};

use crate::decryption::{canonical_host, Decryption};
use tokio_rustls::{
    rustls::{self, pki_types::ServerName},
    LazyConfigAcceptor, TlsConnector,
};

use crate::presentation::Metadata;
use crate::store::{Side, Store};
use crate::{
    Attribution, Counters, Flow, FlowKind, FlowState, Headers, RequestHead, PENDING_ATTRIBUTION,
};

const CONNECTIONS: usize = 256;
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const ATTRIBUTION_TIMEOUT: Duration = Duration::from_millis(250);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

type SharedStore = Arc<Mutex<Store>>;
type MessageBody = UnsyncBoxBody<Bytes, hyper::Error>;
type Finished = Vec<(Metadata, Option<(Headers, Headers)>)>;

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub port: Option<u16>,
    pub allow_local_destinations: bool,
}

enum Command {
    Pause(bool),
    Clear,
    AllowLocal(bool),
    Replay(
        u64,
        crate::ReplayEdits,
        oneshot::Sender<Result<u64, String>>,
    ),
}

/// Owns the dedicated runtime thread. Dropping this handle closes the session.
pub struct Session {
    address: SocketAddr,
    token: String,
    store: SharedStore,
    command: mpsc::Sender<Command>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Session {
    pub fn start(options: Options) -> io::Result<Self> {
        Self::start_inner(options, None)
    }

    pub fn start_with_decryption(
        options: Options,
        decryption: Arc<Decryption>,
    ) -> io::Result<Self> {
        Self::start_inner(options, Some(decryption))
    }

    fn start_inner(options: Options, decryption: Option<Arc<Decryption>>) -> io::Result<Self> {
        let listener = StdListener::bind((Ipv4Addr::LOCALHOST, options.port.unwrap_or(0)))
            .map_err(|error| {
                let address = options.port.filter(|port| *port != 0).map_or_else(
                    || "an OS-assigned port".into(),
                    |port| format!("port {port}. Retry with an OS-assigned port"),
                );
                io::Error::new(
                    error.kind(),
                    format!("Could not listen on {address}: {error}"),
                )
            })?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let token = BASE64_URL_SAFE_NO_PAD.encode(random);
        let auth = format!(
            "Basic {}",
            BASE64_STANDARD.encode(format!("zentop:{token}"))
        );
        let (command, commands) = mpsc::channel(16);
        let (stop, stopped) = oneshot::channel();
        let mut store = Store::default();
        store.counters.allow_local_destinations = options.allow_local_destinations;
        let store = Arc::new(Mutex::new(store));
        let engine = Arc::new(Engine {
            address,
            auth,
            decryption,
            store: store.clone(),
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let listener = {
            let _guard = runtime.enter();
            TcpListener::from_std(listener)?
        };
        let thread = thread::Builder::new()
            .name("zentop-traffic".into())
            .spawn(move || {
                runtime.block_on(engine.run(listener, commands, stopped));
                runtime.shutdown_timeout(STOP_TIMEOUT);
            })?;
        Ok(Self {
            address,
            token,
            store,
            command,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn proxy_url(&self) -> String {
        format!("http://zentop:{}@{}", self.token, self.address)
    }

    pub fn pause(&self, pause: bool) -> bool {
        self.command.try_send(Command::Pause(pause)).is_ok()
    }

    pub fn clear(&self) -> bool {
        self.command.try_send(Command::Clear).is_ok()
    }

    pub fn allow_local_destinations(&self, allow: bool) -> bool {
        self.command.try_send(Command::AllowLocal(allow)).is_ok()
    }

    pub fn har_export(&self, ids: Vec<u64>) -> crate::HarExport {
        crate::HarExport {
            store: Arc::downgrade(&self.store),
            ids,
        }
    }

    pub fn replay_draft(&self, id: u64, reveal: bool) -> Option<crate::ReplayDraft> {
        self.store.lock().unwrap().replay_draft(id, reveal)
    }

    pub fn replay(
        &self,
        id: u64,
        edits: crate::ReplayEdits,
    ) -> Result<oneshot::Receiver<Result<u64, String>>, &'static str> {
        if edits.size() > 1024 * 1024 + 80 * 1024 {
            return Err("Edited request exceeds the capture limits");
        }
        let (reply, result) = oneshot::channel();
        self.command
            .try_send(Command::Replay(id, edits, reply))
            .map_err(|_| "Proxy command queue is busy")?;
        Ok(result)
    }

    pub fn counters(&self) -> Counters {
        self.store.lock().unwrap().counters
    }

    pub fn rows_since(&self, revision: &mut u64) -> Option<crate::RowUpdates> {
        self.store.lock().unwrap().updates(revision)
    }

    pub fn flow(&self, id: u64, show_secrets: bool) -> Option<Flow> {
        self.store.lock().unwrap().flow(id, show_secrets)
    }

    /// Returns a new detail snapshot only when the selected flow has changed.
    pub fn flow_since(&self, id: u64, reveal: bool, revision: Option<u64>) -> Option<(u64, Flow)> {
        self.store.lock().unwrap().flow_since(id, reveal, revision)
    }

    pub fn take_finished(&self, headers: bool) -> Finished {
        let mut store = self.store.lock().unwrap();
        store.drain_events();
        store.finished(headers)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Engine {
    address: SocketAddr,
    auth: String,
    decryption: Option<Arc<Decryption>>,
    store: SharedStore,
}

struct Connection {
    id: u64,
    owner: watch::Receiver<Attribution>,
    attribution: watch::Sender<Attribution>,
    first_request: AtomicBool,
    activity: watch::Sender<Activity>,
    // A tunnel or upgrade keeps its slot after Hyper yields the upgraded socket.
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl Connection {
    async fn wait_for_attribution(&self, store: &SharedStore) -> Option<Duration> {
        if !self.first_request.swap(false, Ordering::Relaxed) {
            return None;
        }
        if *self.owner.borrow() != PENDING_ATTRIBUTION {
            return Some(Duration::ZERO);
        }

        let mut owner = self.owner.clone();
        let started = TokioInstant::now();
        let timed_out = time::timeout(
            ATTRIBUTION_TIMEOUT,
            owner.wait_for(|owner| *owner != PENDING_ATTRIBUTION),
        )
        .await
        .is_err();
        let elapsed = started.elapsed();
        if timed_out {
            crate::attribution::resolve(
                &mut store.lock().unwrap(),
                self.id,
                &self.attribution,
                Attribution::unknown("Process attribution timed out"),
            );
        }
        Some(elapsed)
    }
}

#[derive(Clone, Copy)]
enum Activity {
    WaitingForAuth(TokioInstant),
    Active(usize),
    Idle(TokioInstant),
}

struct ActiveRequest(watch::Sender<Activity>);

impl ActiveRequest {
    fn begin(activity: &watch::Sender<Activity>) -> Self {
        activity.send_modify(|state| {
            *state = match *state {
                Activity::Active(count) => Activity::Active(count + 1),
                _ => Activity::Active(1),
            };
        });
        Self(activity.clone())
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.send_modify(|state| {
            *state = match *state {
                Activity::Active(count) if count > 1 => Activity::Active(count - 1),
                _ => Activity::Idle(TokioInstant::now() + IDLE_TIMEOUT),
            };
        });
    }
}

async fn expire_client(mut activity: watch::Receiver<Activity>) {
    loop {
        let state = *activity.borrow_and_update();
        let deadline = match state {
            Activity::WaitingForAuth(deadline) | Activity::Idle(deadline) => Some(deadline),
            Activity::Active(_) => None,
        };
        if let Some(deadline) = deadline {
            tokio::select! {
                biased;
                result = activity.changed() => if result.is_err() { return; },
                () = time::sleep_until(deadline) => return,
            }
        } else if activity.changed().await.is_err() {
            return;
        }
    }
}

impl Engine {
    async fn run(
        self: Arc<Self>,
        listener: TcpListener,
        mut commands: mpsc::Receiver<Command>,
        mut stopped: oneshot::Receiver<()>,
    ) {
        let (pending, receiver) = mpsc::channel(CONNECTIONS);
        let scanner = tokio::spawn(crate::attribution::worker(
            self.address.port(),
            receiver,
            self.store.clone(),
        ));
        let slots = Arc::new(Semaphore::new(CONNECTIONS));
        let replay_slots = Arc::new(Semaphore::new(4));
        let mut clients = JoinSet::new();
        let mut next_id = 0;
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break,
                Some(command) = commands.recv() => match command {
                    Command::Pause(paused) => self.store.lock().unwrap().pause(paused),
                    Command::Clear => self.store.lock().unwrap().clear(),
                    Command::AllowLocal(allow) => self.store.lock().unwrap().counters.allow_local_destinations = allow,
                    Command::Replay(id, edits, reply) => {
                        let Ok(replay_slot) = replay_slots.clone().try_acquire_owned() else {
                            let _ = reply.send(Err("Too many requests are being replayed".into()));
                            continue;
                        };
                        let Ok(slot) = slots.clone().try_acquire_owned() else {
                            let _ = reply.send(Err("Proxy connection limit reached".into()));
                            continue;
                        };
                        let connection_id = next_id;
                        next_id += 1;
                        let engine = self.clone();
                        clients.spawn(async move {
                            let (_slot, _replay_slot) = (slot, replay_slot);
                            let result = time::timeout(IDLE_TIMEOUT, engine.repeat(id, edits, connection_id)).await
                                .unwrap_or_else(|_| Err("Repeated request timed out".into()));
                            let _ = reply.send(result);
                        });
                    },
                },
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
                accepted = listener.accept() => {
                    let Ok((mut stream, peer)) = accepted else {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        continue;
                    };
                    let Ok(slot) = slots.clone().try_acquire_owned() else {
                        self.store.lock().unwrap().refuse();
                        // Never accumulate extra tasks or open sockets while overloaded.
                        let _ = tokio::time::timeout(Duration::from_millis(10), stream.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
                        )).await;
                        continue;
                    };
                    let id = next_id;
                    next_id += 1;
                    let (sender, owner) = watch::channel(PENDING_ATTRIBUTION);
                    if self.store.lock().unwrap().counters.paused {
                        sender.send_replace(Attribution::unknown("Accepted while capture was paused"));
                    } else if pending.try_send((id, peer, sender.clone())).is_err() {
                        sender.send_replace(Attribution::unknown("Attribution queue was full"));
                    }
                    let (activity, deadlines) = watch::channel(Activity::WaitingForAuth(
                        TokioInstant::now() + AUTH_TIMEOUT,
                    ));
                    let connection = Arc::new(Connection {
                        id, owner, attribution: sender, first_request: AtomicBool::new(true),
                        activity, _slot: slot,
                    });
                    let engine = self.clone();
                    clients.spawn(async move {
                        let mut server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                        server.http1().timer(TokioTimer::new()).max_buf_size(128 * 1024)
                            .header_read_timeout(None);
                        server.http2().timer(TokioTimer::new()).enable_connect_protocol()
                            .max_header_list_size(128 * 1024).max_concurrent_streams(32);
                        let http = server.serve_connection_with_upgrades(TokioIo::new(stream), service_fn(move |request| {
                            engine.clone().handle(request, connection.clone())
                        }));
                        tokio::select! {
                            biased;
                            () = expire_client(deadlines) => {}
                            _ = http => {}
                        }
                    });
                }
            }
        }
        drop(listener);
        scanner.abort();
        clients.abort_all();
    }

    async fn handle(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        connection: Arc<Connection>,
    ) -> Result<Response<MessageBody>, std::convert::Infallible> {
        let target = match Target::from_request(&request) {
            Ok(target) => target,
            Err(reason) => return Ok(reply(StatusCode::BAD_REQUEST, reason)),
        };
        if !authorized(&self.auth, request.headers_mut()) {
            self.store.lock().unwrap().refuse();
            let mut response = reply(
                StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                "Zentop proxy credentials required",
            );
            response.headers_mut().insert(
                header::PROXY_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"Zentop\""),
            );
            if request.version() != Version::HTTP_2 {
                response
                    .headers_mut()
                    .insert(header::CONNECTION, HeaderValue::from_static("close"));
            }
            return Ok(response);
        }
        let decrypt = if let Some(tls) = self
            .decryption
            .as_ref()
            .filter(|tls| target.tunnel && tls.selected(&target.host))
        {
            let tls = tls.clone();
            let host = target.host.clone();
            tokio::task::spawn_blocking(move || tls.available(&host))
                .await
                .unwrap_or_default()
        } else {
            false
        };
        let kind = if target.tunnel {
            FlowKind::Tunnel
        } else {
            FlowKind::Http
        };
        let capture = self.capture(
            &request,
            &target,
            &connection,
            if decrypt {
                FlowKind::DecryptedHttps
            } else {
                kind
            },
        );
        let attribution_wait = connection.wait_for_attribution(&self.store).await;
        if let Some(elapsed) = attribution_wait {
            self.store
                .lock()
                .unwrap()
                .waited_for_attribution(capture.id, elapsed);
        }
        if decrypt {
            return Ok(self.decrypt_tunnel(request, target, connection, capture, attribution_wait));
        }
        Ok(self
            .route(
                request.map(BodyExt::boxed_unsync),
                target,
                Some(connection),
                capture,
                None,
            )
            .await)
    }

    fn capture(
        &self,
        request: &Request<impl HttpBody>,
        target: &Target,
        connection: &Connection,
        kind: FlowKind,
    ) -> Arc<Capture> {
        let activity = ActiveRequest::begin(&connection.activity);
        self.begin_capture(
            request,
            target,
            connection.id,
            connection.owner.borrow().clone(),
            kind,
            Some(activity),
        )
    }

    fn begin_capture(
        &self,
        request: &Request<impl HttpBody>,
        target: &Target,
        connection_id: u64,
        owner: Attribution,
        kind: FlowKind,
        activity: Option<ActiveRequest>,
    ) -> Arc<Capture> {
        let mut store = self.store.lock().unwrap();
        let started = Instant::now();
        let id = store.start(
            connection_id,
            owner,
            kind,
            RequestHead {
                method: request.method().to_string(),
                url: if kind == FlowKind::DecryptedHttps
                    && request.method() != Method::CONNECT
                    && request.uri().scheme().is_none()
                {
                    format!(
                        "https://{}{}",
                        // URLs show the default HTTPS port the way browsers do: not at all.
                        target
                            .authority
                            .strip_suffix(":443")
                            .unwrap_or(&target.authority),
                        request.uri().path_and_query().map_or("/", |v| v.as_str())
                    )
                } else {
                    request.uri().to_string()
                },
                host: target.authority.clone(),
                path: if request.method() == Method::CONNECT {
                    String::new()
                } else {
                    request.uri().path().to_owned()
                },
                version: format!("{:?}", request.version()),
                headers: Vec::new(),
            },
            request.headers(),
        );
        store.request_progress(id, request.body().is_end_stream(), false);
        if activity.is_none() {
            store.repeated(id);
        }
        Arc::new(Capture {
            id,
            started,
            store: self.store.clone(),
            _activity: activity,
        })
    }

    async fn repeat(
        &self,
        id: u64,
        edits: crate::ReplayEdits,
        connection_id: u64,
    ) -> Result<u64, String> {
        let request = self.store.lock().unwrap().replay_request(id, edits)?;
        let https = request.uri().scheme_str() == Some("https");
        if request.method() == Method::CONNECT || has_upgrade(request.headers()) {
            return Err("Replay supports HTTP requests without CONNECT or upgrades".into());
        }
        let target = Target::parse(&request, https)?;
        let tls = if https {
            let decryption = self
                .decryption
                .as_ref()
                .filter(|tls| tls.selected(&target.host))
                .ok_or("HTTPS replay requires a host currently on the decrypt list")?;
            Some(decryption.upstream.clone())
        } else {
            None
        };
        let request = request.map(|body| {
            Full::new(body)
                .map_err(|never| match never {})
                .boxed_unsync()
        });
        let capture = self.begin_capture(
            &request,
            &target,
            connection_id,
            Attribution::Known {
                unique_id: 0,
                pid: std::process::id(),
                start_time: 0,
                name: "Zentop".into(),
            },
            if https {
                FlowKind::DecryptedHttps
            } else {
                FlowKind::Http
            },
            None,
        );
        let id = capture
            .id
            .ok_or("Resume capture before replaying a request")?;
        let mut response = self.route(request, target, None, capture, tls).await;
        while let Some(frame) = response.body_mut().frame().await {
            frame.map_err(|error| error.to_string())?;
        }
        if let Some(flow) = self.store.lock().unwrap().flow(id, false) {
            if let Some(reason) = flow.metadata.state.error_reason() {
                return Err(reason.into());
            }
        }
        Ok(id)
    }

    fn decrypt_tunnel(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        target: Target,
        connection: Arc<Connection>,
        capture: Arc<Capture>,
        attribution_wait: Option<Duration>,
    ) -> Response<MessageBody> {
        let upgraded = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let Ok(client) = upgraded.await else {
                capture.finish(FlowState::Failed(
                    "Client could not establish CONNECT tunnel",
                ));
                return;
            };
            let Some(decryption) = self.decryption.as_ref() else {
                return;
            };
            let hello =
                LazyConfigAcceptor::new(rustls::server::Acceptor::default(), TokioIo::new(client));
            let Ok(Ok(hello)) = time::timeout(DIAL_TIMEOUT, hello).await else {
                capture.finish(FlowState::Failed(
                    "Could not read the program's TLS ClientHello",
                ));
                return;
            };
            if hello
                .client_hello()
                .server_name()
                .and_then(|name| canonical_host(name).ok())
                .as_deref()
                != Some(&target.host)
            {
                self.store.lock().unwrap().refuse();
                capture.finish(FlowState::Refused("TLS server name does not match CONNECT"));
                return;
            }
            let leaf = {
                let decryption = decryption.clone();
                let host = target.host.clone();
                tokio::task::spawn_blocking(move || decryption.leaf(&host)).await
            };
            let Ok(Ok(Some(config))) = leaf else {
                capture.finish(FlowState::Failed(
                    "Decryption is unavailable; reconnect to use a tunnel",
                ));
                return;
            };
            let client = match time::timeout(DIAL_TIMEOUT, hello.into_stream(config)).await {
                Ok(Ok(client)) => client,
                Ok(Err(error)) => {
                    let reason = match error
                        .get_ref()
                        .and_then(|e| e.downcast_ref::<rustls::Error>())
                    {
                        Some(rustls::Error::AlertReceived(
                            rustls::AlertDescription::BadCertificate
                            | rustls::AlertDescription::UnsupportedCertificate
                            | rustls::AlertDescription::CertificateRevoked
                            | rustls::AlertDescription::CertificateExpired
                            | rustls::AlertDescription::CertificateUnknown
                            | rustls::AlertDescription::UnknownCA
                            | rustls::AlertDescription::AccessDenied,
                        )) => "The program rejected Zentop's certificate",
                        _ => "The program's TLS handshake failed",
                    };
                    capture.finish(FlowState::Failed(reason));
                    return;
                }
                Err(_) => {
                    capture.finish(FlowState::Failed("The program's TLS handshake timed out"));
                    return;
                }
            };
            // Successful handshakes have per-request rows; retain CONNECT rows for failures.
            self.store.lock().unwrap().discard_active(capture.id);
            drop(capture);
            let h2 = client.get_ref().1.alpn_protocol() == Some(b"h2");
            let deadlines = connection.activity.subscribe();
            let target = Arc::new(target);
            let attribution_wait = Mutex::new(attribution_wait);
            let service = service_fn(move |request| {
                let attribution_wait = attribution_wait.lock().unwrap().take();
                self.clone().handle_inner(
                    request,
                    target.clone(),
                    connection.clone(),
                    attribution_wait,
                )
            });
            let client = TokioIo::new(client);
            if h2 {
                let http = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .timer(TokioTimer::new())
                    .enable_connect_protocol()
                    .max_header_list_size(128 * 1024)
                    .max_concurrent_streams(32)
                    .serve_connection(client, service);
                tokio::select! {
                    () = expire_client(deadlines) => {},
                    _ = http => {},
                }
            } else {
                let http = hyper::server::conn::http1::Builder::new()
                    .timer(TokioTimer::new())
                    .max_buf_size(128 * 1024)
                    .header_read_timeout(None)
                    .serve_connection(client, service)
                    .with_upgrades();
                tokio::select! {
                    () = expire_client(deadlines) => {},
                    _ = http => {},
                }
            }
        });
        reply(StatusCode::OK, "")
    }

    async fn handle_inner(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        target: Arc<Target>,
        connection: Arc<Connection>,
        attribution_wait: Option<Duration>,
    ) -> Result<Response<MessageBody>, std::convert::Infallible> {
        request.headers_mut().remove(header::PROXY_AUTHORIZATION);
        let capture = self.capture(&request, &target, &connection, FlowKind::DecryptedHttps);
        if let Some(elapsed) = attribution_wait {
            self.store
                .lock()
                .unwrap()
                .waited_for_attribution(capture.id, elapsed);
        }
        let extended_connect = request.extensions().get::<hyper::ext::Protocol>().is_some();
        let rejection = if request.method() == Method::CONNECT && !extended_connect {
            Some((
                StatusCode::METHOD_NOT_ALLOWED,
                "Nested CONNECT is not supported",
            ))
        } else if !target.matches_inner(&request) {
            Some((
                StatusCode::MISDIRECTED_REQUEST,
                "Request authority does not match CONNECT",
            ))
        } else if extended_connect {
            Some((
                StatusCode::NOT_IMPLEMENTED,
                "HTTP/2 extended CONNECT is not supported",
            ))
        } else if self
            .decryption
            .as_ref()
            .is_none_or(|tls| !tls.selected(&target.host))
        {
            Some((
                StatusCode::MISDIRECTED_REQUEST,
                "Decryption was disabled; reconnect to use a tunnel",
            ))
        } else {
            None
        };
        if let Some((status, reason)) = rejection {
            self.store.lock().unwrap().refuse();
            capture.fail(status, FlowState::Refused(reason));
            return Ok(reply(status, reason));
        }
        let tls = self.decryption.as_ref().map(|tls| tls.upstream.clone());
        Ok(self
            .route(
                request.map(BodyExt::boxed_unsync),
                target.as_ref().clone(),
                Some(connection),
                capture,
                tls,
            )
            .await)
    }

    async fn route(
        &self,
        request: Request<MessageBody>,
        target: Target,
        connection: Option<Arc<Connection>>,
        capture: Arc<Capture>,
        tls: Option<Arc<rustls::ClientConfig>>,
    ) -> Response<MessageBody> {
        let dial_started = Instant::now();
        let (status, state, message) = match time::timeout(DIAL_TIMEOUT, self.dial(&target)).await {
            Ok(Ok(upstream)) => {
                self.store
                    .lock()
                    .unwrap()
                    .connected(capture.id, dial_started.elapsed());
                if let Some(mut tls) = tls {
                    if request.version() == Version::HTTP_11 && has_upgrade(request.headers()) {
                        Arc::make_mut(&mut tls).alpn_protocols = vec![b"http/1.1".to_vec()];
                    }
                    let tls_started = Instant::now();
                    match connect_tls(tls, &target.host, upstream).await {
                        Ok(stream) => {
                            self.store
                                .lock()
                                .unwrap()
                                .tls(capture.id, tls_started.elapsed());
                            let h2 = stream.get_ref().1.alpn_protocol() == Some(b"h2");
                            return forward_or_fail(
                                request,
                                target,
                                stream,
                                Some(h2),
                                connection,
                                capture,
                            )
                            .await;
                        }
                        Err(error) => {
                            let reason: String = format!("Upstream TLS failed: {error}")
                                .chars()
                                .take(512)
                                .collect();
                            capture.fail(
                                StatusCode::BAD_GATEWAY,
                                FlowState::UpstreamTls(reason.clone()),
                            );
                            return reply(StatusCode::BAD_GATEWAY, reason);
                        }
                    }
                }
                if target.tunnel {
                    return tunnel(request, upstream, connection, capture);
                }
                return forward_or_fail(request, target, upstream, None, connection, capture).await;
            }
            Ok(Err(DialError::Refused)) => {
                self.store.lock().unwrap().refuse();
                let reason = "Destination is blocked by the local address policy";
                (StatusCode::FORBIDDEN, FlowState::Refused(reason), reason)
            }
            Ok(Err(error)) => {
                let reason = match error {
                    DialError::Resolve => "Destination name could not be resolved",
                    _ => "Could not connect to destination",
                };
                (StatusCode::BAD_GATEWAY, FlowState::Failed(reason), reason)
            }
            Err(_) => {
                let reason = "Destination connection timed out";
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    FlowState::Failed(reason),
                    reason,
                )
            }
        };
        capture.fail(status, state);
        reply(status, message)
    }

    async fn dial(&self, target: &Target) -> Result<TcpStream, DialError> {
        let addresses = lookup_host((target.host.as_str(), target.port))
            .await
            .map_err(|_| DialError::Resolve)?;
        let allow_local = self.store.lock().unwrap().counters.allow_local_destinations;
        let mut blocked = false;
        let mut attempted = false;
        for address in addresses {
            if !allowed(address, self.address, allow_local) {
                blocked = true;
                continue;
            }
            attempted = true;
            // Connect to the exact address we checked, without resolving again.
            if let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(address)).await
            {
                return Ok(stream);
            }
        }
        if blocked && !attempted {
            Err(DialError::Refused)
        } else {
            Err(DialError::Connect)
        }
    }
}

fn authorized(expected: &str, headers: &mut HeaderMap) -> bool {
    let mut credentials = headers.get_all(header::PROXY_AUTHORIZATION).iter();
    let matches = credentials
        .next()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|actual| {
            let actual = actual.as_bytes();
            let expected = expected.as_bytes();
            actual.len() == expected.len()
                && actual[..6].eq_ignore_ascii_case(&expected[..6])
                && actual[6..]
                    .iter()
                    .zip(&expected[6..])
                    .fold(0, |diff, (a, b)| diff | (a ^ b))
                    == 0
        })
        && credentials.next().is_none();
    headers.remove(header::PROXY_AUTHORIZATION);
    matches
}

enum DialError {
    Refused,
    Resolve,
    Connect,
}

fn allowed(address: SocketAddr, own: SocketAddr, allow_local: bool) -> bool {
    let ip = address.ip().to_canonical();
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    let is_own = |ip| ip == own.ip() && address.port() == own.port();
    let ipv4 = |ip: Ipv4Addr| {
        !is_own(IpAddr::V4(ip))
            && !ip.is_unspecified()
            && !ip.is_multicast()
            && (allow_local || public_v4(ip))
    };
    match ip {
        IpAddr::V4(ip) => ipv4(ip),
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            !is_own(IpAddr::V6(ip))
                && (allow_local
                    || !(ip.is_loopback()
                        || ip.is_unicast_link_local()
                        || ip.is_unique_local()
                        || segments[0] & 0xffc0 == 0xfec0
                        || segments[..4] == [0x100, 0, 0, 0]))
                && embedded_ipv4(ip).into_iter().flatten().all(ipv4)
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_link_local()
        || ip.is_private()
        || matches!(
            (a, b, c),
            (0 | 240..=255, _, _) | (100, 64..=127, _) | (192, 0, 0) | (198, 18 | 19, _)
        ))
}

fn embedded_ipv4(ip: Ipv6Addr) -> [Option<Ipv4Addr>; 2] {
    let bytes = ip.octets();
    let at = |offset| Ipv4Addr::from(<[u8; 4]>::try_from(&bytes[offset..offset + 4]).unwrap());
    let address = match ip.segments() {
        // IPv4-compatible and the RFC 6052 well-known translation prefix.
        [0, 0, 0, 0, 0, 0, ..] | [0x64, 0xff9b, 0, 0, 0, 0, ..] => at(12),
        // RFC 6052 /48 layout skips the reserved octet at byte 8.
        [0x64, 0xff9b, 1, ..] => Ipv4Addr::new(bytes[6], bytes[7], bytes[9], bytes[10]),
        [0x2002, ..] => at(2),
        // Teredo stores the server in clear and the client with inverted bits.
        [0x2001, 0, ..] => {
            return [Some(at(4)), Some(Ipv4Addr::from(!u32::from(at(12))))];
        }
        _ => return [None, None],
    };
    [Some(address), None]
}

#[derive(Clone)]
struct Target {
    authority: String,
    host: String,
    port: u16,
    tunnel: bool,
}

impl Target {
    fn from_request<B>(request: &Request<B>) -> Result<Self, &'static str> {
        Self::parse(request, false)
    }

    fn parse<B>(request: &Request<B>, https: bool) -> Result<Self, &'static str> {
        let uri = request.uri();
        let tunnel = request.method() == Method::CONNECT;
        if tunnel {
            if uri.scheme().is_some() || uri.path_and_query().is_some() {
                return Err("CONNECT requires a host and port");
            }
        } else if uri.scheme_str() != Some(if https { "https" } else { "http" })
            && !(!https
                && uri.scheme_str() == Some("ws")
                && request.version() == Version::HTTP_11
                && has_upgrade(request.headers()))
        {
            return Err("Use an absolute http:// URL, a ws:// upgrade, or CONNECT");
        }
        let authority = uri
            .authority()
            .ok_or("Proxy request requires an authority")?;
        if tunnel && authority.port_u16().is_none() {
            return Err("CONNECT requires an explicit port");
        }
        if uri
            .path_and_query()
            .is_some_and(|path| path.as_str().len() > 16 * 1024)
            || request.method().as_str().len() > 256
        {
            return Err("Request target is too long");
        }
        let (host, port) = canonical(authority, if https { 443 } else { 80 })?;
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        Ok(Self {
            authority,
            host,
            port,
            tunnel,
        })
    }
    fn matches_inner(&self, request: &Request<Incoming>) -> bool {
        let uri = request.uri();
        let matches = |authority: &Authority| {
            canonical(authority, 443)
                .is_ok_and(|(host, port)| host == self.host && port == self.port)
        };
        if uri.scheme_str().is_some_and(|scheme| scheme != "https")
            || uri.authority().is_some_and(|authority| !matches(authority))
            || !uri.path().starts_with('/')
            || uri
                .path_and_query()
                .is_some_and(|path| path.as_str().len() > 16 * 1024)
            || request.method().as_str().len() > 256
        {
            return false;
        }
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let host = hosts.next();
        if hosts.next().is_some()
            || host.is_some_and(|host| {
                !Authority::try_from(host.as_bytes()).is_ok_and(|authority| matches(&authority))
            })
        {
            return false;
        }
        if request.version() == Version::HTTP_2 {
            uri.authority().is_some() && uri.scheme_str() == Some("https")
        } else {
            host.is_some()
        }
    }
}

fn canonical(authority: &Authority, default_port: u16) -> Result<(String, u16), &'static str> {
    let raw = authority.host();
    if authority.as_str().contains('@')
        || (raw.len() != authority.as_str().len() && authority.port_u16().is_none())
    {
        return Err("Invalid destination authority");
    }
    let host = if let Some(ipv6) = raw.strip_prefix('[').and_then(|ip| ip.strip_suffix(']')) {
        ipv6.parse::<std::net::Ipv6Addr>()
            .map_err(|_| "Invalid IPv6 destination")?
            .to_string()
    } else {
        if let Ok(ip) = raw.parse::<Ipv4Addr>() {
            ip.to_string()
        } else {
            canonical_host(raw)?
        }
    };
    let port = authority.port_u16().unwrap_or(default_port);
    if port == 0 {
        return Err("Invalid destination port");
    }
    Ok((host, port))
}

fn reply(status: StatusCode, text: impl Into<Bytes>) -> Response<MessageBody> {
    let body = Full::new(text.into())
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}

fn header_tokens(headers: &HeaderMap, name: header::HeaderName) -> impl Iterator<Item = &str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(',').map(str::trim))
        .filter(|value| !value.is_empty())
}

fn has_upgrade(headers: &HeaderMap) -> bool {
    header_tokens(headers, header::CONNECTION).any(|token| token.eq_ignore_ascii_case("upgrade"))
        && header_tokens(headers, header::UPGRADE).next().is_some()
}

fn strip_upgrade_headers(headers: &mut HeaderMap, upgrade: bool) {
    let values: Vec<_> = if upgrade {
        headers.get_all(header::UPGRADE).iter().cloned().collect()
    } else {
        Vec::new()
    };
    strip_hop_headers(headers);
    if upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        for value in values {
            headers.append(header::UPGRADE, value);
        }
    }
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let nominated: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| header::HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authorization",
        "proxy-authenticate",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

struct Capture {
    id: Option<u64>,
    started: Instant,
    store: SharedStore,
    _activity: Option<ActiveRequest>,
}

impl Capture {
    fn fail(&self, status: StatusCode, state: FlowState) {
        let mut store = self.store.lock().unwrap();
        let elapsed = self.started.elapsed();
        store.response(self.id, status.as_u16(), &HeaderMap::new(), elapsed);
        store.finish(self.id, state, elapsed);
    }

    fn finish(&self, state: FlowState) {
        self.store
            .lock()
            .unwrap()
            .finish(self.id, state, self.started.elapsed());
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.finish(FlowState::Failed(
            "Connection closed before transfer completed",
        ));
    }
}

struct UpstreamDriver(AbortHandle);
impl Drop for UpstreamDriver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct StreamingBody {
    incoming: MessageBody,
    side: Side,
    capture: Arc<Capture>,
    _driver: Option<UpstreamDriver>,
}

impl HttpBody for StreamingBody {
    type Data = Bytes;
    type Error = hyper::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let result = Pin::new(&mut self.incoming).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(bytes) = frame.data_ref() {
                    self.capture
                        .store
                        .lock()
                        .unwrap()
                        .body(self.capture.id, self.side, bytes);
                }
                if self.side == Side::Request {
                    self.capture.store.lock().unwrap().request_progress(
                        self.capture.id,
                        self.incoming.is_end_stream(),
                        frame.is_trailers(),
                    );
                }
                if self.side == Side::Response && self.incoming.is_end_stream() {
                    self.capture.finish(FlowState::Complete);
                }
            }
            Poll::Ready(Some(Err(_))) => self
                .capture
                .finish(FlowState::Failed("HTTP body transfer failed")),
            Poll::Ready(None) if self.side == Side::Response => {
                self.capture.finish(FlowState::Complete)
            }
            Poll::Ready(None) => {
                self.capture
                    .store
                    .lock()
                    .unwrap()
                    .request_progress(self.capture.id, true, false)
            }
            _ => {}
        }
        result.map(|frame| {
            frame.map(|frame| {
                frame.map(|mut frame| {
                    if let Some(trailers) = frame.trailers_mut() {
                        strip_hop_headers(trailers);
                    }
                    frame
                })
            })
        })
    }

    fn is_end_stream(&self) -> bool {
        self.incoming.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.incoming.size_hint()
    }
}

async fn connect_tls(
    config: Arc<rustls::ClientConfig>,
    host: &str,
    stream: TcpStream,
) -> io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let name = ServerName::try_from(host.to_owned()).map_err(io::Error::other)?;
    time::timeout(
        DIAL_TIMEOUT,
        TlsConnector::from(config).connect(name, stream),
    )
    .await?
}

async fn forward_or_fail(
    request: Request<MessageBody>,
    target: Target,
    upstream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    tls_h2: Option<bool>,
    connection: Option<Arc<Connection>>,
    capture: Arc<Capture>,
) -> Response<MessageBody> {
    match forward(
        request,
        target,
        upstream,
        tls_h2,
        connection,
        capture.clone(),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            let reason = "Upstream HTTP request failed";
            capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
            reply(StatusCode::BAD_GATEWAY, reason)
        }
    }
}

async fn forward(
    mut request: Request<MessageBody>,
    target: Target,
    upstream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    tls_h2: Option<bool>,
    connection: Option<Arc<Connection>>,
    capture: Arc<Capture>,
) -> Result<Response<MessageBody>, Box<dyn std::error::Error + Send + Sync>> {
    let version = request.version();
    let upgrade = connection.is_some()
        && tls_h2 != Some(true)
        && version == Version::HTTP_11
        && has_upgrade(request.headers());
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
    let protocols = if upgrade {
        header_tokens(request.headers(), header::UPGRADE)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let (mut head, incoming) = request.into_parts();
    strip_upgrade_headers(&mut head.headers, upgrade);
    let path = head.uri.path_and_query().map_or("/", |path| path.as_str());
    if tls_h2 == Some(true) {
        head.uri = format!("https://{}{path}", target.authority).parse()?;
        head.version = Version::HTTP_2;
        head.headers.remove(header::HOST);
    } else {
        head.uri = path.parse()?;
        head.version = Version::HTTP_11;
        head.headers
            .insert(header::HOST, HeaderValue::from_str(&target.authority)?);
    }
    let request = Request::from_parts(
        head,
        StreamingBody {
            incoming,
            side: Side::Request,
            capture: capture.clone(),
            _driver: None,
        },
    );
    let offers_websocket = protocols
        .iter()
        .any(|protocol| protocol.eq_ignore_ascii_case("websocket"));
    let mut extensions = HeaderMap::new();
    if offers_websocket {
        for value in request.headers().get_all("sec-websocket-extensions") {
            extensions.append("sec-websocket-extensions", value.clone());
        }
    }
    let (mut response, driver) = if tls_h2 == Some(true) {
        let (mut sender, upstream) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(upstream))
                .await?;
        let driver = UpstreamDriver(tokio::spawn(upstream).abort_handle());
        (sender.send_request(request).await?, driver)
    } else {
        let (mut sender, upstream) =
            hyper::client::conn::http1::handshake(TokioIo::new(upstream)).await?;
        let driver = UpstreamDriver(tokio::spawn(upstream.with_upgrades()).abort_handle());
        (sender.send_request(request).await?, driver)
    };
    let upgraded = response.status() == StatusCode::SWITCHING_PROTOCOLS;
    if upgraded
        && (!has_upgrade(response.headers())
            || header_tokens(response.headers(), header::UPGRADE)
                .next()
                .is_none()
            || !header_tokens(response.headers(), header::UPGRADE).all(|selected| {
                protocols
                    .iter()
                    .any(|offered| offered.eq_ignore_ascii_case(selected))
            }))
    {
        return Err(io::Error::other("Origin selected an unsolicited upgrade").into());
    }
    {
        let mut store = capture.store.lock().unwrap();
        store.response(
            capture.id,
            response.status().as_u16(),
            response.headers(),
            capture.started.elapsed(),
        );
        store.response_version(capture.id, response.version());
    }
    if let Some(client) = client_upgrade.filter(|_| upgraded) {
        let compression = (offers_websocket
            && header_tokens(response.headers(), header::UPGRADE)
                .any(|value| value.eq_ignore_ascii_case("websocket")))
        .then(|| crate::websocket::Compression::negotiated(&extensions, response.headers()));
        let server = hyper::upgrade::on(&mut response);
        capture
            .store
            .lock()
            .unwrap()
            .upgraded(capture.id, compression.is_some());
        tokio::spawn(async move {
            let _connection = connection;
            let _driver = driver;
            match tokio::try_join!(client, server) {
                Ok((client, server)) => {
                    relay_pair(
                        TokioIo::new(client),
                        TokioIo::new(server),
                        &capture,
                        compression,
                    )
                    .await
                }
                Err(_) => capture.finish(FlowState::Failed("Could not establish HTTP upgrade")),
            }
        });
        let (mut head, _) = response.into_parts();
        strip_upgrade_headers(&mut head.headers, true);
        return Ok(Response::from_parts(
            head,
            reply(StatusCode::OK, "").into_body(),
        ));
    }
    let (mut head, incoming) = response.into_parts();
    strip_hop_headers(&mut head.headers);
    head.version = version;
    if incoming.is_end_stream() {
        capture.finish(FlowState::Complete);
    }
    Ok(Response::from_parts(
        head,
        StreamingBody {
            incoming: incoming.boxed_unsync(),
            side: Side::Response,
            capture,
            _driver: Some(driver),
        }
        .boxed_unsync(),
    ))
}

fn tunnel(
    mut request: Request<MessageBody>,
    upstream: TcpStream,
    connection: Option<Arc<Connection>>,
    capture: Arc<Capture>,
) -> Response<MessageBody> {
    let upgraded = hyper::upgrade::on(&mut request);
    capture.store.lock().unwrap().response(
        capture.id,
        200,
        &HeaderMap::new(),
        capture.started.elapsed(),
    );
    tokio::spawn(async move {
        let _connection = connection;
        let Ok(client) = upgraded.await else {
            capture.finish(FlowState::Failed(
                "Client could not establish CONNECT tunnel",
            ));
            return;
        };
        relay_pair(TokioIo::new(client), upstream, &capture, None).await;
    });
    reply(StatusCode::OK, "")
}

async fn relay_pair(
    client: impl AsyncRead + AsyncWrite + Unpin,
    server: impl AsyncRead + AsyncWrite + Unpin,
    capture: &Capture,
    compression: Option<crate::websocket::Compression>,
) {
    let inspectors = |direction| {
        Some(crate::websocket::Inspector::new(
            capture.store.clone(),
            capture.id?,
            direction,
            compression?,
        ))
    };
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let result = tokio::try_join!(
        relay(
            client_read,
            server_write,
            capture,
            Side::Request,
            inspectors(crate::Direction::ClientToServer)
        ),
        relay(
            server_read,
            client_write,
            capture,
            Side::Response,
            inspectors(crate::Direction::ServerToClient)
        ),
    );
    capture.finish(if result.is_ok() {
        FlowState::Complete
    } else {
        FlowState::Failed("Relayed connection transfer failed")
    });
}

async fn relay(
    mut source: impl AsyncRead + Unpin,
    mut target: impl AsyncWrite + Unpin,
    capture: &Capture,
    side: Side,
    mut inspector: Option<crate::websocket::Inspector>,
) -> io::Result<()> {
    let mut buffer = [0u8; 8192];
    loop {
        let read = source.read(&mut buffer).await?;
        if read == 0 {
            return target.shutdown().await;
        }
        let mut offset = 0;
        while offset < read {
            let written = target.write(&buffer[offset..read]).await?;
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            if let Some(inspector) = &mut inspector {
                inspector.feed(&buffer[offset..offset + written]);
            }
            offset += written;
            capture
                .store
                .lock()
                .unwrap()
                .count(capture.id, side, written as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_require_proxy_form_and_normalize_authority() {
        assert!(Target::from_request(&Request::builder().uri("/").body(()).unwrap()).is_err());
        let target = Target::from_request(
            &Request::builder()
                .uri("http://EXAMPLE.COM./path")
                .body(())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(target.authority, "example.com:80");
        assert_eq!(
            idna::domain_to_ascii_strict("bücher.example").unwrap(),
            "xn--bcher-kva.example"
        );
        assert!(canonical(&"example.com:abc".parse().unwrap(), 80).is_err());
    }

    #[test]
    fn credentials_are_always_consumed() {
        let mut headers = HeaderMap::new();
        assert!(!authorized("Basic abc", &mut headers));
        headers.insert(
            header::PROXY_AUTHORIZATION,
            HeaderValue::from_static("Basic def"),
        );
        assert!(!authorized("Basic abc", &mut headers));
        assert!(headers.is_empty());
        headers.insert(
            header::PROXY_AUTHORIZATION,
            HeaderValue::from_static("basic abc"),
        );
        assert!(authorized("Basic abc", &mut headers));
        assert!(headers.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn unresolved_attribution_times_out_without_blocking_later_requests() {
        let (attribution, owner) = watch::channel(PENDING_ATTRIBUTION);
        let connection = Arc::new(Connection {
            id: 0,
            owner,
            attribution,
            first_request: AtomicBool::new(true),
            activity: watch::channel(Activity::Active(1)).0,
            _slot: Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap(),
        });
        let store = Arc::new(Mutex::new(Store::default()));
        // Leave the watch pending, as if the scanner stalled.
        let first = tokio::spawn({
            let connection = connection.clone();
            let store = store.clone();
            async move { connection.wait_for_attribution(&store).await }
        });
        tokio::task::yield_now().await;
        time::advance(Duration::from_millis(249)).await;
        assert!(!first.is_finished());
        let before = TokioInstant::now();
        assert_eq!(connection.wait_for_attribution(&store).await, None);
        assert_eq!(before.elapsed(), Duration::ZERO);
        assert_eq!(first.await.unwrap(), Some(Duration::from_millis(250)));
        assert_eq!(
            *connection.owner.borrow(),
            Attribution::Unknown("Process attribution timed out")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn authenticated_activity_extends_deadline_until_last_request_ends() {
        let (activity, deadlines) =
            watch::channel(Activity::WaitingForAuth(TokioInstant::now() + AUTH_TIMEOUT));
        let expiry = tokio::spawn(expire_client(deadlines));
        time::advance(AUTH_TIMEOUT - Duration::from_secs(1)).await;
        let upload = ActiveRequest::begin(&activity);
        let download = ActiveRequest::begin(&activity);
        drop(upload);
        time::advance(2 * IDLE_TIMEOUT).await;
        assert!(!expiry.is_finished());
        drop(download);
        let idle_since = TokioInstant::now();
        expiry.await.unwrap();
        assert_eq!(idle_since.elapsed(), IDLE_TIMEOUT);
    }

    #[test]
    fn rejects_local_and_mapped_addresses() {
        let own = "127.0.0.1:8890".parse().unwrap();
        for local in [
            "127.0.0.1:80",
            "10.1.2.3:80",
            "169.254.1.3:80",
            "100.64.0.0:80",
            "100.127.255.255:80",
            "192.0.0.255:80",
            "198.18.0.0:80",
            "198.19.255.255:80",
            "240.0.0.0:80",
            "255.255.255.254:80",
            "255.255.255.255:80",
            "[::ffff:127.0.0.1]:80",
            "[fe80::1]:80",
            "[fd00::1]:80",
            "[fec0::1]:80",
            "[feff::1]:80",
            "[100::1]:80",
            "[64:ff9b::100.64.0.1]:80",
            "[64:ff9b:1:c000:0:700::]:80",
            "[2002:c612:1::1]:80",
            "[2001:0:a00:1:0:0:fefe:fefe]:80",
            "[2001:0:101:101:0:0:f5ff:fffe]:80",
            "[::192.0.0.1]:80",
        ] {
            let address = local.parse().unwrap();
            assert!(!allowed(address, own, false));
            assert!(allowed(address, own, true));
        }
        assert!(!allowed(own, own, true));
        assert!(!allowed(
            "[::ffff:127.0.0.1]:8890".parse().unwrap(),
            own,
            true
        ));
        for public in [
            "1.1.1.1:80",
            "100.128.0.0:80",
            "198.20.0.0:80",
            "[64:ff9b::1.1.1.1]:80",
            "[64:ff9b:1:101:1:100::]:80",
            "[2002:101:101::1]:80",
            "[2001:0:101:101:0:0:f7f7:f7f7]:80",
            "[::1.1.1.1]:80",
        ] {
            assert!(allowed(public.parse().unwrap(), own, false), "{public}");
        }
    }
}
