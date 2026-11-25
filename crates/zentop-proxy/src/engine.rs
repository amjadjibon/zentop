use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use base64::{prelude::*, Engine as _};
use bytes::Bytes;
use http::header::{self, HeaderValue};
use http::uri::Authority;
use http::{HeaderMap, Method, Request, Response, StatusCode, Uri, Version};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body as HttpBody, Frame, Incoming, SizeHint};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{lookup_host, TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use tokio::task::{AbortHandle, JoinSet};

use crate::presentation::Metadata;
use crate::store::{Side, Store};
use crate::{Attribution, Counters, Flow, FlowKind, FlowState, Headers, RequestHead};

const CONNECTIONS: usize = 256;
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const PENDING: Attribution = Attribution::Unknown("Attribution pending");

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

    pub fn counters(&self) -> Counters {
        self.store.lock().unwrap().counters
    }

    pub fn rows_since(&self, revision: &mut u64) -> Option<crate::RowUpdates> {
        self.store.lock().unwrap().updates(revision)
    }

    pub fn flow(&self, id: u64, show_secrets: bool) -> Option<Flow> {
        self.store.lock().unwrap().flow(id, show_secrets)
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
    store: SharedStore,
}

struct Connection {
    id: u64,
    owner: watch::Receiver<Attribution>,
    // A CONNECT stream keeps its slot after Hyper yields the upgraded socket.
    _slot: tokio::sync::OwnedSemaphorePermit,
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
                    let (sender, owner) = watch::channel(PENDING);
                    if self.store.lock().unwrap().counters.paused {
                        sender.send_replace(Attribution::unknown("Accepted while capture was paused"));
                    } else if let Err(error) = pending.try_send((id, peer, sender)) {
                        error.into_inner().2.send_replace(Attribution::unknown("Attribution queue was full"));
                    }
                    let connection = Arc::new(Connection { id, owner, _slot: slot });
                    let engine = self.clone();
                    clients.spawn(async move {
                        let mut server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                        server.http1().timer(TokioTimer::new()).max_buf_size(128 * 1024)
                            .header_read_timeout(Duration::from_secs(10));
                        server.http2().timer(TokioTimer::new()).enable_connect_protocol()
                            .max_header_list_size(128 * 1024).max_concurrent_streams(32);
                        let _ = server.serve_connection_with_upgrades(TokioIo::new(stream), service_fn(move |request| {
                            engine.clone().handle(request, connection.clone())
                        })).await;
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
            return Ok(response);
        }
        let started = Instant::now();
        let process = connection.owner.borrow().clone();
        let kind = if target.tunnel {
            FlowKind::Tunnel
        } else {
            FlowKind::Http
        };
        let id = {
            let mut store = self.store.lock().unwrap();
            let id = store.start(
                connection.id,
                process.clone(),
                kind,
                RequestHead {
                    method: request.method().to_string(),
                    url: request.uri().to_string(),
                    host: target.authority.clone(),
                    path: if target.tunnel {
                        String::new()
                    } else {
                        request.uri().path().to_owned()
                    },
                    version: format!("{:?}", request.version()),
                    headers: Vec::new(),
                },
                request.headers(),
            );
            if id.is_some() && process != PENDING && matches!(process, Attribution::Unknown(_)) {
                store.counters.unattributed += 1;
            }
            id
        };
        let capture = Arc::new(Capture {
            id,
            started,
            store: self.store.clone(),
        });
        if !target.tunnel && request.headers().contains_key(header::UPGRADE) {
            self.store.lock().unwrap().refuse();
            capture.fail(
                StatusCode::BAD_REQUEST,
                FlowState::Refused("Use CONNECT for upgraded protocols"),
            );
            return Ok(reply(
                StatusCode::BAD_REQUEST,
                "Use CONNECT for upgraded protocols",
            ));
        }
        let dial_started = Instant::now();
        let upstream = tokio::time::timeout(DIAL_TIMEOUT, self.dial(&target)).await;
        let upstream = match upstream {
            Ok(Ok(stream)) => stream,
            Ok(Err(DialError::Refused)) => {
                self.store.lock().unwrap().refuse();
                capture.fail(
                    StatusCode::FORBIDDEN,
                    FlowState::Refused("Destination is blocked by the local address policy"),
                );
                return Ok(reply(
                    StatusCode::FORBIDDEN,
                    "Destination is blocked by the local address policy",
                ));
            }
            Ok(Err(error)) => {
                let reason = match error {
                    DialError::Resolve => "Destination name could not be resolved",
                    _ => "Could not connect to destination",
                };
                capture.fail(StatusCode::BAD_GATEWAY, FlowState::Failed(reason));
                return Ok(reply(StatusCode::BAD_GATEWAY, reason));
            }
            Err(_) => {
                capture.fail(
                    StatusCode::GATEWAY_TIMEOUT,
                    FlowState::Failed("Destination connection timed out"),
                );
                return Ok(reply(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Destination connection timed out",
                ));
            }
        };
        self.store
            .lock()
            .unwrap()
            .connected(id, dial_started.elapsed());
        if target.tunnel {
            return Ok(tunnel(request, upstream, connection, capture));
        }

        match forward(request, target, upstream, capture.clone()).await {
            Ok(response) => Ok(response),
            Err(_) => {
                capture.fail(
                    StatusCode::BAD_GATEWAY,
                    FlowState::Failed("Upstream HTTP request failed"),
                );
                Ok(reply(
                    StatusCode::BAD_GATEWAY,
                    "Upstream HTTP request failed",
                ))
            }
        }
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
    if (ip == own.ip() || ip.is_unspecified()) && address.port() == own.port() {
        return false;
    }
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    allow_local
        || match ip {
            IpAddr::V4(ip) => {
                !(ip.is_loopback()
                    || ip.is_link_local()
                    || ip.is_private()
                    || ip.is_broadcast()
                    || ip.octets()[0] == 0)
            }
            IpAddr::V6(ip) => {
                !(ip.is_loopback() || ip.is_unicast_link_local() || ip.is_unique_local())
            }
        }
}

struct Target {
    authority: String,
    host: String,
    port: u16,
    tunnel: bool,
}

impl Target {
    fn from_request<B>(request: &Request<B>) -> Result<Self, &'static str> {
        let uri = request.uri();
        let tunnel = request.method() == Method::CONNECT;
        if tunnel {
            if uri.scheme().is_some() || uri.path_and_query().is_some() {
                return Err("CONNECT requires a host and port");
            }
        } else if uri.scheme_str() != Some("http") {
            return Err("Use an absolute http:// URL or CONNECT");
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
        let (host, port) = canonical(authority)?;
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
}

fn canonical(authority: &Authority) -> Result<(String, u16), &'static str> {
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
        let host = idna::domain_to_ascii_strict(raw.strip_suffix('.').unwrap_or(raw))
            .map_err(|_| "Invalid destination hostname")?;
        if host.is_empty() || host.len() > 253 {
            return Err("Invalid destination hostname");
        }
        host.to_ascii_lowercase()
    };
    let port = authority.port_u16().unwrap_or(80);
    if port == 0 {
        return Err("Invalid destination port");
    }
    Ok((host, port))
}

fn reply(status: StatusCode, text: &'static str) -> Response<MessageBody> {
    let body = Full::new(Bytes::from_static(text.as_bytes()))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
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
    incoming: Incoming,
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

async fn forward(
    request: Request<Incoming>,
    target: Target,
    upstream: TcpStream,
    capture: Arc<Capture>,
) -> Result<Response<MessageBody>, Box<dyn std::error::Error + Send + Sync>> {
    let (mut head, incoming) = request.into_parts();
    let http2 = head.version == Version::HTTP_2;
    strip_hop_headers(&mut head.headers);
    let path = head.uri.path_and_query().map_or("/", |path| path.as_str());
    if http2 {
        head.uri = Uri::builder()
            .scheme("http")
            .authority(target.authority)
            .path_and_query(path)
            .build()?;
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
    let (response, driver) = if http2 {
        send_h2(upstream, request).await?
    } else {
        send_h1(upstream, request).await?
    };
    let (mut head, incoming) = response.into_parts();
    capture.store.lock().unwrap().response(
        capture.id,
        head.status.as_u16(),
        &head.headers,
        capture.started.elapsed(),
    );
    strip_hop_headers(&mut head.headers);
    if incoming.is_end_stream() {
        capture.finish(FlowState::Complete);
    }
    Ok(Response::from_parts(
        head,
        StreamingBody {
            incoming,
            side: Side::Response,
            capture,
            _driver: Some(driver),
        }
        .boxed_unsync(),
    ))
}

async fn send_h1(
    stream: TcpStream,
    request: Request<StreamingBody>,
) -> Result<(Response<Incoming>, UpstreamDriver), hyper::Error> {
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .handshake(TokioIo::new(stream))
        .await?;
    let driver = drive(connection);
    Ok((sender.send_request(request).await?, driver))
}
async fn send_h2(
    stream: TcpStream,
    request: Request<StreamingBody>,
) -> Result<(Response<Incoming>, UpstreamDriver), hyper::Error> {
    let (mut sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .handshake(TokioIo::new(stream))
        .await?;
    let driver = drive(connection);
    Ok((sender.send_request(request).await?, driver))
}
fn drive(task: impl Future<Output = Result<(), hyper::Error>> + Send + 'static) -> UpstreamDriver {
    UpstreamDriver(tokio::spawn(task).abort_handle())
}

fn tunnel(
    mut request: Request<Incoming>,
    upstream: TcpStream,
    connection: Arc<Connection>,
    capture: Arc<Capture>,
) -> Response<MessageBody> {
    // This is the handoff point for a later TLS engine. Phase 1 only relays bytes.
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
        let (client_read, client_write) = tokio::io::split(TokioIo::new(client));
        let (server_read, server_write) = upstream.into_split();
        let result = tokio::try_join!(
            relay(client_read, server_write, &capture, Side::Request),
            relay(server_read, client_write, &capture, Side::Response),
        );
        capture.finish(if result.is_ok() {
            FlowState::Complete
        } else {
            FlowState::Failed("CONNECT tunnel transfer failed")
        });
    });
    reply(StatusCode::OK, "")
}

async fn relay(
    mut source: impl AsyncRead + Unpin,
    mut target: impl AsyncWrite + Unpin,
    capture: &Capture,
    side: Side,
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
        assert!(canonical(&"example.com:abc".parse().unwrap()).is_err());
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

    #[test]
    fn rejects_local_and_mapped_addresses() {
        let own = "127.0.0.1:8890".parse().unwrap();
        for local in [
            "127.0.0.1:80",
            "10.1.2.3:80",
            "169.254.1.3:80",
            "[::ffff:127.0.0.1]:80",
            "[fe80::1]:80",
            "[fd00::1]:80",
        ] {
            let address = local.parse().unwrap();
            assert!(!allowed(address, own, false));
            assert!(allowed(address, own, true));
        }
        assert!(!allowed(own, own, true));
        assert!(allowed("1.1.1.1:80".parse().unwrap(), own, false));
    }
}
