use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body as WireBody, Frame};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::{rustls, TlsAcceptor, TlsConnector};
use zentop_proxy::{
    Attribution, Body, Direction, FlowKind, FlowState, HarOptions, Opcode, Options, ReplayEdits,
    ReplayHeader, ReplayValue, Session,
};

fn proxy(local: bool) -> Session {
    Session::start(Options {
        allow_local_destinations: local,
        ..Options::default()
    })
    .unwrap()
}

fn auth(session: &Session) -> String {
    format!(
        "Basic {}",
        STANDARD.encode(format!("zentop:{}", session.token()))
    )
}

async fn client(
    address: SocketAddr,
) -> (
    hyper::client::conn::http1::SendRequest<Full<Bytes>>,
    JoinHandle<()>,
) {
    let io = TokioIo::new(TcpStream::connect(address).await.unwrap());
    let (sender, connection) = hyper::client::conn::http1::handshake(io).await.unwrap();
    let task = tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    (sender, task)
}

async fn wait(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition in time");
}

async fn fast_origin() -> (SocketAddr, JoinHandle<()>) {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = origin.accept().await {
            tokio::spawn(async move {
                let service = service_fn(|_| async {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (address, server)
}

#[cfg(target_os = "macos")]
async fn curl_flows(count: u64) -> Vec<Duration> {
    use std::process::{Command, Stdio};

    let (address, origin) = fast_origin().await;
    let session = proxy(true);
    let mut waits = Vec::new();
    for id in 0..count {
        let url = format!("http://{address}/{id}");
        let mut child = Command::new("/usr/bin/curl")
            .args([
                "-q",
                "-fsS",
                "--max-time",
                "5",
                "--noproxy",
                "",
                "-x",
                &session.proxy_url(),
                &url,
            ])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(tokio::task::spawn_blocking(move || child.wait().unwrap())
            .await
            .unwrap()
            .success());
        let mut finished = Vec::new();
        wait(|| {
            finished.extend(session.take_finished(false));
            !finished.is_empty()
        })
        .await;
        let [(row, _)] = finished.as_slice() else {
            panic!("expected one finished curl request");
        };
        assert_eq!(row.id, id);
        assert!(
            matches!(&row.process, Attribution::Known { pid: owner_pid, name, .. }
            if *owner_pid == pid && name == "curl"),
            "request {id}: expected curl pid {pid}, got {:?}",
            row.process
        );
        waits.push(row.timing.attribution.expect("first request timing"));
    }
    origin.abort();
    waits
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn attributes_twenty_short_lived_curl_clients() {
    curl_flows(20).await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "measures first-request attribution wait for 100 sequential curl requests"]
async fn benchmark_curl_attribution_wait() {
    let mut waits = curl_flows(100).await;
    waits.sort_unstable();
    let median = (waits[49] + waits[50]) / 2;
    eprintln!(
        "100 sequential curl requests: median {:.3} ms, p95 {:.3} ms",
        median.as_secs_f64() * 1_000.,
        waits[94].as_secs_f64() * 1_000.,
    );
}

#[tokio::test]
async fn only_first_request_on_keep_alive_connection_waits_for_attribution() {
    let (address, origin) = fast_origin().await;
    let session = proxy(true);
    let (mut sender, _) = client(session.address()).await;
    for id in 0..2 {
        let response = sender
            .send_request(
                Request::builder()
                    .uri(format!("http://{address}/{id}"))
                    .header("proxy-authorization", auth(&session))
                    .body(Full::default())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.collect().await.unwrap().to_bytes(), "ok");
    }
    let first = session.flow(0, false).unwrap().metadata;
    let second = session.flow(1, false).unwrap().metadata;
    assert_eq!(first.connection, second.connection);
    assert_eq!(first.process, second.process);
    assert!(first.timing.attribution.is_some());
    assert_eq!(second.timing.attribution, None);
    origin.abort();
}

#[tokio::test]
async fn auth_origin_policy_forwarding_pause_and_redaction() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = origin.accept().await {
            tokio::spawn(async move {
                let service = service_fn(|request: Request<hyper::body::Incoming>| async move {
                    assert!(!request.headers().contains_key("proxy-authorization"));
                    assert_eq!(request.uri().path(), "/test");
                    assert_eq!(request.uri().query(), Some("api_key=private&city=Oslo"));
                    assert_eq!(request.headers()["authorization"], "Bearer confidential");
                    assert_eq!(request.headers()["x-api-key"], "wire-key");
                    assert_eq!(request.headers()["host"], address.to_string());
                    assert_eq!(request.collect().await.unwrap().to_bytes(), "payload");
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header("set-cookie", "secret=1")
                            .header("x-csrf-token", "wire-csrf")
                            .body(Full::new(Bytes::from_static(b"upstream data")))
                            .unwrap(),
                    )
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    let session = proxy(false);
    let (mut sender, _driver) = client(session.address()).await;
    let template = |url: &str| {
        Request::builder()
            .method("POST")
            .uri(url)
            .header("host", "wrong.example")
    };
    let url = format!(
        "http://localhost:{}/test?api_key=private&city=Oslo",
        address.port()
    );
    let response = sender
        .send_request(template("/test").body(Full::default()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    response.collect().await.unwrap();
    let response = sender
        .send_request(
            template(&url)
                .header("proxy-authorization", "Basic bad")
                .body(Full::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PROXY_AUTHENTICATION_REQUIRED);
    assert_eq!(response.headers()["connection"], "close");
    response.collect().await.unwrap();
    assert!(sender
        .send_request(template(&url).body(Full::default()).unwrap())
        .await
        .is_err());
    let (mut sender, _driver) = client(session.address()).await;
    let response = sender
        .send_request(
            template(&url)
                .header("proxy-authorization", auth(&session))
                .body(Full::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    response.collect().await.unwrap();
    assert_eq!(session.counters().refused, 2);
    let refused = session.flow(0, false).unwrap();
    assert!(matches!(refused.metadata.state, FlowState::Refused(_)));
    assert_eq!(refused.metadata.status, Some(403));

    assert!(session.allow_local_destinations(true));
    wait(|| session.counters().allow_local_destinations).await;
    let send = || {
        template(&url.replacen("localhost", "127.0.0.1", 1))
            .header("proxy-authorization", auth(&session))
            .header("authorization", "Bearer confidential")
            .header("x-api-key", "wire-key")
            .body(Full::new(Bytes::from_static(b"payload")))
            .unwrap()
    };
    let response = sender.send_request(send()).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.collect().await.unwrap().to_bytes(),
        "upstream data"
    );
    wait(|| {
        session
            .flow(1, false)
            .is_some_and(|flow| flow.metadata.state == FlowState::Complete)
    })
    .await;

    let flow = session.flow(1, false).unwrap();
    assert!(flow.request.url.contains("api_key=[redacted]"));
    assert_eq!(flow.metadata.path, "/test");
    assert!(flow
        .request
        .headers
        .contains(&("x-api-key".into(), "[redacted]".into())));
    assert!(flow
        .response
        .as_ref()
        .unwrap()
        .headers
        .contains(&("x-csrf-token".into(), "[redacted]".into())));
    let curl = zentop_proxy::copy_as_curl(&flow, false).unwrap();
    assert!(!curl.contains("confidential"));
    assert!(!curl.contains("api_key"));
    assert!(!curl.contains("x-api-key"));
    assert!(curl.contains("city=Oslo"));
    assert!(curl.contains("payload"));
    let revealed = session.flow(1, true).unwrap();
    let revealed_curl = zentop_proxy::copy_as_curl(&revealed, true).unwrap();
    assert!(revealed_curl.contains("Bearer confidential"));
    assert!(revealed_curl.contains("x-api-key: wire-key"));
    assert!(revealed
        .response
        .as_ref()
        .unwrap()
        .headers
        .contains(&("x-csrf-token".into(), "wire-csrf".into())));
    assert!(!revealed
        .request
        .headers
        .iter()
        .any(|(name, _)| name == "proxy-authorization"));

    let mut finished = Vec::new();
    wait(|| {
        finished.extend(session.take_finished(true));
        finished.len() == 2
    })
    .await;
    let (row, headers) = &finished[1];
    let json = serde_json::to_string(&(row, headers)).unwrap();
    for secret in [
        "confidential",
        "wire-key",
        "wire-csrf",
        "secret=1",
        "api_key",
        "private",
        session.token(),
    ] {
        assert!(!json.contains(secret));
    }
    #[cfg(target_os = "macos")]
    assert!(matches!(row.process, Attribution::Known { pid, .. } if pid == std::process::id()));
    #[cfg(not(target_os = "macos"))]
    assert!(matches!(row.process, Attribution::Unknown(_)));

    assert!(session.pause(true));
    wait(|| session.counters().paused).await;
    assert_eq!(
        sender
            .send_request(send())
            .await
            .unwrap()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
        "upstream data"
    );
    assert!(session.flow(2, false).is_none());
    assert!(session.counters().sent >= 14);
    assert!(session.clear());
    wait(|| session.counters().memory == 0).await;
    server.abort();
}

#[tokio::test]
async fn translates_http2_to_http11_origin() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = origin.accept().await.unwrap();
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(move |request: Request<hyper::body::Incoming>| async move {
                    assert_eq!(request.version(), http::Version::HTTP_11);
                    assert_eq!(request.uri().path(), "/h2");
                    assert_eq!(request.headers()["host"], address.to_string());
                    assert!(!request.headers().contains_key("proxy-authorization"));
                    assert_eq!(request.collect().await.unwrap().to_bytes(), "h2 request");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                        b"h2 response",
                    ))))
                }),
            )
            .await
            .ok();
    });
    let session = proxy(true);
    let stream = TcpStream::connect(session.address()).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::handshake::<_, _, Full<Bytes>>(
        TokioExecutor::new(),
        TokioIo::new(stream),
    )
    .await
    .unwrap();
    tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .uri(format!("http://{address}/h2"))
                .header("proxy-authorization", auth(&session))
                .header("host", "wrong.example")
                .body(Full::new(Bytes::from_static(b"h2 request")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.version(), http::Version::HTTP_2);
    assert_eq!(response.collect().await.unwrap().to_bytes(), "h2 response");
}

const CLIENT_FRAME: &[u8] = &[
    0x81,
    0x85,
    1,
    2,
    3,
    4,
    b'h' ^ 1,
    b'e' ^ 2,
    b'l' ^ 3,
    b'l' ^ 4,
    b'o' ^ 1,
];
const SERVER_FRAME: &[u8] = b"\x81\x05hello";

fn websocket_echo(request: &mut Request<hyper::body::Incoming>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let upgrade = hyper::upgrade::on(request);
    tokio::spawn(async move {
        let mut stream = TokioIo::new(upgrade.await.unwrap());
        let mut bytes = [0; CLIENT_FRAME.len()];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, CLIENT_FRAME);
        stream.write_all(SERVER_FRAME).await.unwrap();
        stream.shutdown().await.unwrap();
        // Keep the write half available until the peer finishes its TLS shutdown.
        assert_eq!(stream.read(&mut bytes).await.unwrap(), 0);
    });
}

async fn exercise_websocket(session: &Session, id: u64, response: Response<hyper::body::Incoming>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(response.headers()["upgrade"], "websocket");
    let mut stream = TokioIo::new(hyper::upgrade::on(response).await.unwrap());
    stream.write_all(CLIENT_FRAME).await.unwrap();
    let mut bytes = [0; SERVER_FRAME.len()];
    stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(bytes, SERVER_FRAME);
    stream.shutdown().await.unwrap();
    assert_eq!(stream.read(&mut bytes).await.unwrap(), 0);
    drop(stream);
    wait(|| session.flow(id, false).unwrap().metadata.state == FlowState::Complete).await;
    let flow = session.flow(id, false).unwrap();
    assert_eq!(
        flow.metadata.bytes,
        (CLIENT_FRAME.len() as u64, SERVER_FRAME.len() as u64)
    );
    assert_eq!(flow.metadata.kind, FlowKind::Upgrade);
    assert_eq!(flow.message_count, 2);
    for (message, expected) in flow
        .messages
        .iter()
        .zip([Direction::ClientToServer, Direction::ServerToClient])
    {
        assert_eq!(message.direction, expected);
        assert_eq!(message.opcode, Opcode::Text);
        assert_eq!(message.size, 5);
        assert_eq!(message.payload, b"hello");
    }
}

#[tokio::test]
async fn forwards_http11_upgrades_and_regular_origin_responses() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for switching in [false, true] {
            let (stream, _) = origin.accept().await.unwrap();
            hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(
                        move |mut request: Request<hyper::body::Incoming>| async move {
                            assert_eq!(request.uri().path(), "/socket");
                            assert_eq!(request.headers()["upgrade"], "websocket");
                            assert_eq!(request.headers()["connection"], "upgrade");
                            assert!(!request.headers().contains_key("x-hop"));
                            assert!(!request.headers().contains_key("proxy-authorization"));
                            let mut response = Response::builder().header("upgrade", "websocket");
                            if switching {
                                websocket_echo(&mut request);
                                response = response.status(101).header("connection", "upgrade");
                            }
                            let bytes = if switching {
                                Bytes::new()
                            } else {
                                Bytes::from_static(b"ordinary response")
                            };
                            Ok::<_, Infallible>(response.body(Full::new(bytes)).unwrap())
                        },
                    ),
                )
                .with_upgrades()
                .await
                .unwrap();
        }
    });
    let session = proxy(false);
    let request = || {
        Request::builder()
            .uri(format!("ws://{address}/socket"))
            .header("connection", "Upgrade, x-hop")
            .header("upgrade", "websocket")
            .header("x-hop", "must not forward")
            .header("proxy-authorization", auth(&session))
            .body(Full::default())
            .unwrap()
    };
    let (mut sender, _) = client(session.address()).await;
    let response = sender.send_request(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    response.collect().await.unwrap();

    assert!(session.allow_local_destinations(true));
    wait(|| session.counters().allow_local_destinations).await;
    let response = sender.send_request(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("upgrade"));
    assert_eq!(
        response.collect().await.unwrap().to_bytes(),
        "ordinary response"
    );
    assert_eq!(
        session.flow(1, false).unwrap().metadata.kind,
        FlowKind::Http
    );

    let (mut sender, _) = client(session.address()).await;
    let response = sender.send_request(request()).await.unwrap();
    exercise_websocket(&session, 2, response).await;
    let flow = session.flow(2, false).unwrap();
    assert_eq!(flow.metadata.kind, FlowKind::Upgrade);
    assert_eq!(flow.metadata.status, Some(101));
    assert!(flow.metadata.timing.attribution.is_some());
    assert!(flow.metadata.timing.connect.is_some());
    assert!(flow.metadata.timing.total >= flow.metadata.timing.ttfb.unwrap());
    for body in [flow.request_body, flow.response_body] {
        assert!(matches!(
            body,
            Body::Skipped("Upgraded connection; see Messages for WebSocket traffic")
        ));
    }
    server.await.unwrap();
}

#[tokio::test]
async fn tunnel_carries_verified_tls_without_capturing_plaintext() {
    // A throwaway certificate and trust store exist only inside this test process.
    let key = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert = key.cert.der().clone();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.signing_key.serialize_der()).into(),
        )
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let tls_client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = origin.accept().await.unwrap();
        let secured = TlsAcceptor::from(Arc::new(tls))
            .accept(stream)
            .await
            .unwrap();
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(secured),
                service_fn(|request: Request<hyper::body::Incoming>| async move {
                    assert_eq!(request.uri().path(), "/encrypted");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"inside TLS"))))
                }),
            )
            .await
            .ok();
    });

    let session = proxy(true);
    let (mut sender, _driver) = client(session.address()).await;
    let response = sender
        .send_request(
            Request::builder()
                .method("CONNECT")
                .uri(address.to_string())
                .header("proxy-authorization", auth(&session))
                .body(Full::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let upgrade = hyper::upgrade::on(response).await.unwrap();
    let secured = TlsConnector::from(Arc::new(tls_client))
        .connect("localhost".try_into().unwrap(), TokioIo::new(upgrade))
        .await
        .unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(secured))
            .await
            .unwrap();
    tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .uri("/encrypted")
                .header("host", "localhost")
                .header("connection", "close")
                .body(Full::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.collect().await.unwrap().to_bytes(), "inside TLS");
    drop(sender);
    server.await.unwrap();
    wait(|| {
        session
            .flow(0, false)
            .is_some_and(|flow| flow.metadata.state != FlowState::Active)
    })
    .await;
    let flow = session.flow(0, false).unwrap();
    assert_eq!(flow.metadata.kind, FlowKind::Tunnel);
    assert_eq!(flow.metadata.state, FlowState::Complete);
    assert!(flow.metadata.bytes.0 > 10 && flow.metadata.bytes.1 > 10);
    assert!(flow.metadata.timing.attribution.is_some());
    assert!(matches!(flow.response_body, Body::Skipped(_)));
    assert!(session.flow(1, false).is_none());
}

struct Stream(usize);

impl WireBody for Stream {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        static CHUNK: [u8; 64 * 1024] = [b'x'; 64 * 1024];
        if self.0 == 0 {
            Poll::Ready(None)
        } else {
            self.0 -= 1;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(&CHUNK)))))
        }
    }
}

#[tokio::test]
async fn forwards_stream_larger_than_capture_budget() {
    const BLOCKS: usize = 65 * 16;
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = origin.accept().await.unwrap();
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(|_| async { Ok::<_, Infallible>(Response::new(Stream(BLOCKS))) }),
            )
            .await
            .ok();
    });
    let session = proxy(true);
    let (mut sender, _driver) = client(session.address()).await;
    let response = sender
        .send_request(
            Request::builder()
                .uri(format!("http://{address}/large"))
                .header("proxy-authorization", auth(&session))
                .body(Full::default())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    let mut received = 0;
    while let Some(frame) = body.frame().await {
        received += frame.unwrap().into_data().unwrap().len();
        assert!(session.counters().memory < 2 * 1024 * 1024);
    }
    assert_eq!(received, BLOCKS * 64 * 1024);
    let flow = session.flow(0, false).unwrap();
    assert!(matches!(flow.response_body, Body::Truncated { kept, total }
        if kept.len() == 1024 * 1024 && total == received as u64));
}

#[tokio::test]
async fn caps_connections_and_closes_listener_and_clients_on_stop() {
    use tokio::io::AsyncReadExt;

    let session = proxy(false);
    let address = session.address();
    let mut clients = Vec::new();
    for _ in 0..256 {
        clients.push(TcpStream::connect(address).await.unwrap());
    }
    let mut extra = TcpStream::connect(address).await.unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(3), extra.read_to_string(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert!(reply.starts_with("HTTP/1.1 503"));
    assert_eq!(session.counters().refused, 1);

    let start = std::time::Instant::now();
    drop(session);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(TcpStream::connect(address).await.is_err());
    let mut last = clients.pop().unwrap();
    assert_eq!(last.read(&mut [0]).await.unwrap(), 0);
}

#[tokio::test]
async fn unauthenticated_idle_connections_release_their_slots() {
    use tokio::io::AsyncReadExt;

    let session = proxy(false);
    let address = session.address();
    let started = tokio::time::Instant::now();
    let mut clients = Vec::new();
    for _ in 0..256 {
        clients.push(TcpStream::connect(address).await.unwrap());
    }
    let mut extra = TcpStream::connect(address).await.unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(3), extra.read_to_string(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert!(reply.starts_with("HTTP/1.1 503"));

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(15), clients[0].read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(started.elapsed() >= Duration::from_secs(10));
    let (mut sender, _) = client(address).await;
    let reply = sender
        .send_request(
            Request::builder()
                .uri("http://127.0.0.1:80/")
                .header("proxy-authorization", auth(&session))
                .body(Full::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
}

#[test]
fn occupied_port_error_names_port_and_new_sessions_get_new_tokens() {
    let session = proxy(false);
    let port = session.address().port();
    let second = Session::start(Options {
        port: Some(port),
        ..Options::default()
    });
    let error = second.err().expect("occupied port");
    assert!(error.to_string().contains(&format!("port {port}")));
    assert!(error.to_string().contains("OS-assigned"));
    assert_ne!(session.token(), proxy(false).token());
}

type ProxyTlsStream = tokio_rustls::client::TlsStream<TokioIo<hyper::upgrade::Upgraded>>;

fn tls_client(
    certificate: Option<rustls::pki_types::CertificateDer<'static>>,
    alpn: &[u8],
) -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    if let Some(certificate) = certificate {
        roots.add(certificate).unwrap();
    }
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    config
}

struct DecryptedTest {
    session: Session,
    port: u16,
    root: rustls::pki_types::CertificateDer<'static>,
}

impl DecryptedTest {
    async fn new(origin_h2: bool, trust_origin: bool, blocks: usize) -> Self {
        Self::with_origin(origin_h2, trust_origin, blocks, false).await
    }

    async fn with_origin(
        origin_h2: bool,
        trust_origin: bool,
        blocks: usize,
        websocket: bool,
    ) -> Self {
        let origin_cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let origin_trust = trust_origin.then(|| origin_cert.cert.der().clone());
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![origin_cert.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(origin_cert.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        tls.alpn_protocols = if websocket {
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        } else if origin_h2 {
            vec![b"h2".to_vec()]
        } else {
            vec![b"http/1.1".to_vec()]
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(tls) = TlsAcceptor::from(Arc::new(tls)).accept(stream).await else {
                return;
            };
            let server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            let http = service_fn(
                move |mut request: Request<hyper::body::Incoming>| async move {
                    if websocket {
                        assert_eq!(request.version(), http::Version::HTTP_11);
                        assert_eq!(request.uri().path(), "/echo");
                        websocket_echo(&mut request);
                        return Ok::<_, Infallible>(
                            Response::builder()
                                .status(101)
                                .header("connection", "upgrade")
                                .header("upgrade", "websocket")
                                .body(Stream(0))
                                .unwrap(),
                        );
                    }
                    assert_eq!(
                        request.version(),
                        if origin_h2 {
                            http::Version::HTTP_2
                        } else {
                            http::Version::HTTP_11
                        }
                    );
                    assert_eq!(request.uri().path(), "/capture");
                    assert!(request.headers().get("proxy-authorization").is_none());
                    assert_eq!(
                        request.into_body().collect().await.unwrap().to_bytes(),
                        "request body"
                    );
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header("set-cookie", "session=secret")
                            .header("x-origin", "local TLS origin")
                            .body(Stream(blocks))
                            .unwrap(),
                    )
                },
            );
            let _ = server
                .serve_connection_with_upgrades(TokioIo::new(tls), http)
                .await;
        });

        let mut ca = rcgen::CertificateParams::default();
        ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        ca.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = ca.self_signed(&key).unwrap();
        let authority = zentop_proxy::CertificateAuthority::new(
            certificate.der(),
            key,
            time::OffsetDateTime::now_utc() + time::Duration::days(365),
        )
        .unwrap();
        let decryption = Arc::new(zentop_proxy::Decryption::with_client_config(tls_client(
            origin_trust,
            b"h2",
        )));
        decryption.set_authority(Some(authority));
        decryption.set_hosts(["LOCALHOST.".to_owned()]);
        let session = Session::start_with_decryption(
            Options {
                allow_local_destinations: true,
                ..Options::default()
            },
            decryption,
        )
        .unwrap();
        Self {
            session,
            port,
            root: certificate.der().clone(),
        }
    }

    async fn connect(
        &self,
        sni: &'static str,
        h2: bool,
        trust_proxy: bool,
    ) -> std::io::Result<ProxyTlsStream> {
        let (mut sender, _driver) = client(self.session.address()).await;
        let response = sender
            .send_request(
                Request::builder()
                    .method("CONNECT")
                    .uri(format!("LOCALHOST.:{}", self.port))
                    .header("proxy-authorization", auth(&self.session))
                    .body(Full::default())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let upgrade = hyper::upgrade::on(response).await.unwrap();
        let protocol: &[u8] = if h2 { b"h2" } else { b"http/1.1" };
        let roots = trust_proxy.then(|| self.root.clone());
        TlsConnector::from(Arc::new(tls_client(roots, protocol)))
            .connect(sni.try_into().unwrap(), TokioIo::new(upgrade))
            .await
    }

    fn post(&self, h2: bool) -> Request<Full<Bytes>> {
        let authority = format!("localhost:{}", self.port);
        let uri = if h2 {
            format!("https://{authority}/capture?token=secret")
        } else {
            "/capture?token=secret".to_owned()
        };
        Request::post(uri)
            .header("host", authority)
            .header("authorization", "secret")
            .header("proxy-authorization", "must not be captured")
            .body(Full::new(Bytes::from_static(b"request body")))
            .unwrap()
    }
}

async fn check_decrypted_stream(h2: bool, origin_h2: bool, blocks: usize) {
    let test = DecryptedTest::new(origin_h2, true, blocks).await;
    let tls = test.connect("localhost", h2, true).await.unwrap();
    assert_eq!(
        tls.get_ref().1.alpn_protocol().unwrap(),
        if h2 { b"h2".as_slice() } else { b"http/1.1" }
    );
    let response = if h2 {
        let (mut sender, driver) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
                .await
                .unwrap();
        tokio::spawn(driver);
        sender.send_request(test.post(true)).await.unwrap()
    } else {
        let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .unwrap();
        tokio::spawn(driver);
        sender.send_request(test.post(false)).await.unwrap()
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-origin"], "local TLS origin");
    let mut body = response.into_body();
    let mut total = 0;
    while let Some(frame) = body.frame().await {
        total += frame.unwrap().into_data().unwrap().len();
        assert!(test.session.counters().memory < 2 * 1024 * 1024);
    }
    assert_eq!(total, blocks * 64 * 1024);
    wait(|| {
        test.session
            .flow(1, false)
            .is_some_and(|flow| flow.metadata.state == FlowState::Complete)
    })
    .await;
    assert!(test.session.flow(0, false).is_none());
    let flow = test.session.flow(1, false).unwrap();
    assert_eq!(flow.metadata.kind, FlowKind::DecryptedHttps);
    assert_eq!(flow.metadata.status, Some(200));
    assert!(flow.metadata.timing.tls.is_some());
    assert!(!flow.request.url.contains("secret"));
    assert_eq!(
        flow.request
            .headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .unwrap()
            .1,
        "[redacted]"
    );
    assert!(!flow
        .request
        .headers
        .iter()
        .any(|(name, _)| name == "proxy-authorization"));
    assert_eq!(flow.request_body.kept(), b"request body");
    assert_eq!(
        flow.response
            .unwrap()
            .headers
            .iter()
            .find(|(name, _)| name == "set-cookie")
            .unwrap()
            .1,
        "[redacted]"
    );
    if total > 1024 * 1024 {
        assert!(
            matches!(flow.response_body, Body::Truncated { kept, total: bytes }
            if kept.len() == 1024 * 1024 && bytes == total as u64)
        );
    } else {
        assert_eq!(flow.response_body.kept().len(), total);
    }
}

#[tokio::test]
async fn decrypted_tls_negotiates_both_http_versions_on_each_leg() {
    for client_h2 in [false, true] {
        for origin_h2 in [false, true] {
            check_decrypted_stream(client_h2, origin_h2, 1).await;
        }
    }
}

#[tokio::test]
async fn decrypted_stream_larger_than_the_capture_budget_still_forwards() {
    check_decrypted_stream(true, true, 65 * 16).await;
}

#[tokio::test]
async fn decrypted_connection_enforces_names_on_requests_and_upgrades() {
    let test = DecryptedTest::new(false, true, 1).await;
    assert!(test.connect("other.example", false, true).await.is_err());
    let mismatched = test.session.flow(0, false).unwrap();
    assert!(matches!(
        mismatched.metadata.state,
        FlowState::Refused("TLS server name does not match CONNECT")
    ));
    let tls = test.connect("localhost", false, true).await.unwrap();
    let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(driver);
    for (request, status) in [
        (Request::get("/").header("host", "other.example"), 421),
        (Request::get("/").header("host", "localhost"), 421),
        (
            Request::get(format!("https://other.example:{}/", test.port))
                .header("host", format!("localhost:{}", test.port)),
            421,
        ),
        (
            Request::get("/")
                .header("host", format!("localhost:{}", test.port))
                .header("host", "other.example"),
            421,
        ),
        (
            Request::builder()
                .method("CONNECT")
                .uri(format!("localhost:{}", test.port)),
            405,
        ),
        (
            Request::get("/")
                .header("host", "other.example")
                .header("connection", "upgrade")
                .header("upgrade", "websocket"),
            421,
        ),
    ] {
        let response = sender
            .send_request(request.body(Full::<Bytes>::default()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        response.collect().await.unwrap();
    }
    let tls = test.connect("localhost", true, true).await.unwrap();
    let (mut sender, driver) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .unwrap();
    tokio::spawn(driver);
    for req in [
        Request::get(format!("https://other.example:{}/", test.port)),
        Request::get(format!("https://localhost:{}/", test.port)).header("host", "other.example"),
    ] {
        let response = sender
            .send_request(req.body(Full::<Bytes>::default()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 421);
        response.collect().await.unwrap();
    }
    assert_eq!(test.session.counters().refused, 9);
}

#[tokio::test]
async fn upstream_tls_failure_and_client_certificate_rejection_are_visible() {
    let test = DecryptedTest::new(false, false, 1).await;
    assert!(test.connect("localhost", false, false).await.is_err());
    wait(|| {
        test.session.flow(0, false).is_some_and(|flow| {
            flow.metadata.state == FlowState::Failed("The program rejected Zentop's certificate")
        })
    })
    .await;
    let tls = test.connect("localhost", false, true).await.unwrap();
    let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(driver);
    let response = sender.send_request(test.post(false)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        matches!(test.session.flow(2, false).unwrap().metadata.state,
        FlowState::UpstreamTls(reason) if reason.contains("UnknownIssuer"))
    );
}

#[tokio::test]
async fn relays_and_records_websocket_after_tls_termination() {
    let test = DecryptedTest::with_origin(false, true, 0, true).await;
    let tls = test.connect("localhost", false, true).await.unwrap();
    let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(driver.with_upgrades());
    let response = sender
        .send_request(
            Request::get("/echo")
                .header("host", format!("localhost:{}", test.port))
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .body(Full::<Bytes>::default())
                .unwrap(),
        )
        .await
        .unwrap();
    exercise_websocket(&test.session, 1, response).await;
    assert!(test
        .session
        .flow(1, false)
        .unwrap()
        .metadata
        .timing
        .tls
        .is_some());
}

#[tokio::test]
async fn repeats_and_edits_captured_requests_and_enforces_policy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, mut received) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        for _ in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            let sent = sent.clone();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |request: Request<hyper::body::Incoming>| {
                            let sent = sent.clone();
                            async move {
                                let (head, body) = request.into_parts();
                                let body = body.collect().await.unwrap().to_bytes();
                                sent.send((head, body)).await.unwrap();
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                                    b"origin reply",
                                ))))
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    let session = proxy(true);
    let (mut sender, _) = client(session.address()).await;
    let original = sender
        .send_request(
            Request::post(format!(
                "http://{address}/capture?token=original&api_key=keep"
            ))
            .header("proxy-authorization", auth(&session))
            .header("authorization", "Bearer actual")
            .header("cookie", "hello=world")
            .header("x-opaque", http::HeaderValue::from_bytes(&[0xff]).unwrap())
            .body(Full::new(Bytes::from_static(b"original body")))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(original.collect().await.unwrap().to_bytes(), "origin reply");
    let (head, _) = received.recv().await.unwrap();
    wait(|| session.flow(0, false).unwrap().replayable).await;
    let draft = session.replay_draft(0, false).unwrap();
    assert!(!draft.url.contains("original"));
    assert_eq!(
        draft
            .headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .unwrap()
            .1,
        "[redacted]"
    );
    assert!(session
        .replay_draft(0, true)
        .unwrap()
        .url
        .contains("original"));

    let repeated = session
        .replay(0, ReplayEdits::default())
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let (same, body) = received.recv().await.unwrap();
    assert_eq!(same.method, head.method);
    assert_eq!(same.uri, head.uri);
    assert_eq!(same.headers["authorization"], "Bearer actual");
    assert_eq!(same.headers["x-opaque"].as_bytes(), &[0xff]);
    assert!(!same.headers.contains_key("proxy-authorization"));
    assert_eq!(body, "original body");
    let flow = session.flow(repeated, false).unwrap();
    assert!(flow.metadata.repeated);
    assert_eq!(flow.metadata.process.name(), "Zentop");
    assert_eq!(flow.response_body.kept(), b"origin reply");

    let edited = session
        .replay(
            0,
            ReplayEdits {
                method: Some("PATCH".into()),
                url: Some(format!(
                    "http://{address}/edited?token=new&api_key=[redacted]"
                )),
                preserve_masked_query: true,
                body: Some("updated body".into()),
                headers: Some(
                    draft
                        .headers
                        .iter()
                        .enumerate()
                        .map(|(i, (name, _))| ReplayHeader {
                            name: name.clone(),
                            value: if name == "authorization" {
                                ReplayValue::Edited("[redacted]".into())
                            } else {
                                ReplayValue::Original(i)
                            },
                        })
                        .collect(),
                ),
            },
        )
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let (head, body) = received.recv().await.unwrap();
    assert_eq!(head.method, "PATCH");
    assert_eq!(head.uri, "/edited?token=new&api_key=keep");
    assert_eq!(head.headers["authorization"], "[redacted]");
    assert_eq!(head.headers["cookie"], "hello=world");
    assert_eq!(head.headers["content-length"], "12");
    assert_eq!(body, "updated body");
    assert!(session.flow(edited, false).unwrap().metadata.repeated);

    assert!(session.allow_local_destinations(false));
    let blocked = session
        .replay(0, ReplayEdits::default())
        .unwrap()
        .await
        .unwrap()
        .unwrap_err();
    assert!(blocked.contains("local address policy"));
    assert!(matches!(
        session.flow(3, false).unwrap().metadata.state,
        FlowState::Refused(_)
    ));
    let unlisted = session
        .replay(
            0,
            ReplayEdits {
                url: Some("https://example.com/".into()),
                ..ReplayEdits::default()
            },
        )
        .unwrap()
        .await
        .unwrap()
        .unwrap_err();
    assert!(unlisted.contains("decrypt list"));

    for include_bodies in [false, true] {
        let mut output = Vec::new();
        assert_eq!(
            session
                .har_export(vec![0, repeated, edited])
                .write(&mut output, HarOptions { include_bodies })
                .unwrap(),
            3
        );
        let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(parsed["log"]["version"], "1.2");
        assert_eq!(parsed["log"]["creator"]["name"], "Zentop");
        let entries = parsed["log"]["entries"].as_array().unwrap();
        let first = &entries[0];
        assert_eq!(first["request"]["method"], "POST");
        assert_eq!(first["response"]["status"], 200);
        assert_eq!(first["request"]["queryString"][0]["value"], "[redacted]");
        assert_eq!(first["request"]["queryString"][1]["value"], "[redacted]");
        let notes = String::from_utf8(output).unwrap();
        assert!(!notes.contains("Bearer actual"));
        assert!(!notes.contains("hello=world"));
        assert!(!notes.contains("api_key=keep"));
        assert!(!notes.contains("token=original"));
        assert!(!notes.contains(session.token()));
        assert_eq!(
            first["request"]["postData"]["text"].as_str(),
            include_bodies.then_some("original body")
        );
        assert_eq!(
            first["response"]["content"]["text"].as_str(),
            include_bodies.then_some("origin reply")
        );
        assert!(first["startedDateTime"].as_str().unwrap().ends_with('Z'));
        for field in ["send", "wait", "receive"] {
            assert!(first["timings"][field].as_f64().unwrap() >= 0.);
        }
    }
}

// Phase 3B: live editing.

use zentop_proxy::rules::{Action, RewriteAction, Rule, RuleSide};
use zentop_proxy::{HeldEdits, RuleKind, RuleSet};

fn rule(id: u32, url: &str, action: Action) -> Rule {
    Rule {
        id,
        name: format!("Rule {id}"),
        enabled: true,
        method: None,
        url: url.into(),
        app: None,
        action,
    }
}

/// An origin that describes each request in its body and header.
async fn echo_origin() -> (SocketAddr, JoinHandle<()>) {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = origin.accept().await {
            tokio::spawn(async move {
                let service = service_fn(|request: Request<hyper::body::Incoming>| async move {
                    let (head, body) = request.into_parts();
                    let body = body.collect().await.unwrap().to_bytes();
                    let text = format!(
                        "{} {} x-test={} body={}",
                        head.method,
                        head.uri,
                        head.headers
                            .get("x-test")
                            .map_or("-", |value| value.to_str().unwrap()),
                        String::from_utf8_lossy(&body)
                    );
                    let mut response = Response::builder()
                        .header("x-origin", "echo")
                        .header("content-type", "text/plain");
                    let body = if head.uri.path().ends_with("gzip") {
                        response = response.header("content-encoding", "gzip");
                        let mut gzip =
                            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                        std::io::Write::write_all(&mut gzip, text.as_bytes()).unwrap();
                        gzip.finish().unwrap()
                    } else {
                        text.into_bytes()
                    };
                    Ok::<_, Infallible>(response.body(Full::new(Bytes::from(body))).unwrap())
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (address, server)
}

/// Sends one request on its own connection.
async fn send(
    session: &Session,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &'static str,
) -> (StatusCode, http::HeaderMap, Bytes) {
    let (mut sender, _driver) = client(session.address()).await;
    let mut request = Request::builder()
        .method(method)
        .uri(url)
        .header("proxy-authorization", auth(session));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = sender
        .send_request(
            request
                .body(Full::new(Bytes::from_static(body.as_bytes())))
                .unwrap(),
        )
        .await
        .unwrap();
    let (head, body) = response.into_parts();
    (
        head.status,
        head.headers,
        body.collect().await.unwrap().to_bytes(),
    )
}

async fn complete(session: &Session, id: u64) -> zentop_proxy::Flow {
    wait(|| {
        session
            .flow(id, false)
            .is_some_and(|flow| flow.metadata.state != FlowState::Active)
    })
    .await;
    session.flow(id, false).unwrap()
}

#[tokio::test]
async fn request_breakpoint_edits_and_executes_while_other_connections_flow() {
    let (address, origin) = echo_origin().await;
    let session = proxy(true);
    session.set_rules(
        RuleSet::new(
            vec![rule(
                1,
                &format!("http://{address}/held*"),
                Action::Breakpoint {
                    request: true,
                    response: false,
                },
            )],
            true,
        )
        .unwrap(),
    );
    let held = tokio::spawn({
        let url = format!("http://{address}/held?token=secret");
        let session_auth = auth(&session);
        let proxy_address = session.address();
        async move {
            let (mut sender, _driver) = client(proxy_address).await;
            let response = sender
                .send_request(
                    Request::post(url)
                        .header("proxy-authorization", session_auth)
                        .header("authorization", "Bearer original")
                        .header("x-test", "before")
                        .body(Full::new(Bytes::from_static(b"original")))
                        .unwrap(),
                )
                .await
                .unwrap();
            response.collect().await.unwrap().to_bytes()
        }
    });
    wait(|| session.held_count() == 1).await;

    // Holding one request never blocks another connection.
    let (status, _, body) = tokio::time::timeout(
        Duration::from_secs(5),
        send(&session, "GET", &format!("http://{address}/free"), &[], ""),
    )
    .await
    .expect("other connections keep flowing");
    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with(b"GET /free"));

    let summary = session.held_messages().remove(0);
    assert!(!summary.response);
    assert!(!summary.url.contains("secret"));
    let message = session.held_message(summary.id, false).unwrap();
    assert_eq!(message.method, "POST");
    assert_eq!(message.body.as_deref(), Some("original"));
    let authorization = message
        .headers
        .iter()
        .position(|(name, _)| name == "authorization")
        .unwrap();
    assert_eq!(message.headers[authorization].1, "[redacted]");
    assert!(session
        .execute_held(
            summary.id,
            HeldEdits {
                method: Some("BAD METHOD".into()),
                ..HeldEdits::default()
            }
        )
        .is_err());
    let headers = message
        .headers
        .iter()
        .enumerate()
        .map(|(index, (name, _))| ReplayHeader {
            name: name.clone(),
            value: if name == "x-test" {
                ReplayValue::Edited("after".into())
            } else {
                ReplayValue::Original(index)
            },
        })
        .collect();
    session
        .execute_held(
            summary.id,
            HeldEdits {
                method: Some("PUT".into()),
                url: Some(message.url.replace("/held", "/held/edited")),
                preserve_masked_query: true,
                headers: Some(headers),
                body: Some("edited body".into()),
                ..HeldEdits::default()
            },
        )
        .unwrap();
    let body = held.await.unwrap();
    assert_eq!(
        body,
        "PUT /held/edited?token=secret x-test=after body=edited body"
    );
    let flow = complete(&session, 0).await;
    assert_eq!(flow.metadata.method, "PUT");
    assert_eq!(flow.metadata.path, "/held/edited");
    assert_eq!(flow.request_body.kept(), b"edited body");
    assert_eq!(flow.metadata.rules, vec![(1, RuleKind::Breakpoint)]);
    assert!(
        flow.rules[0].note.contains("executed with edits"),
        "{:?}",
        flow.rules
    );
    assert_eq!(session.held_count(), 0);
    origin.abort();
}

#[tokio::test]
async fn response_breakpoints_abort_time_out_and_release_on_pause() {
    let (address, origin) = echo_origin().await;
    let session = proxy(true);
    session.set_rules(
        RuleSet::new(
            vec![
                rule(
                    1,
                    &format!("http://{address}/abort"),
                    Action::Breakpoint {
                        request: false,
                        response: true,
                    },
                ),
                rule(
                    2,
                    &format!("http://{address}/slow"),
                    Action::Breakpoint {
                        request: true,
                        response: false,
                    },
                ),
            ],
            true,
        )
        .unwrap()
        .with_hold_timeout(Duration::from_millis(300)),
    );

    // An aborted response is replaced by a 502.
    let abort_url = format!("http://{address}/abort");
    let request = send(&session, "GET", &abort_url, &[], "");
    let abort = async {
        wait(|| session.held_count() == 1).await;
        let held = session.held_messages().remove(0);
        assert!(held.response);
        assert_eq!(held.status, Some(200));
        let message = session.held_message(held.id, false).unwrap();
        assert!(message.body.unwrap().starts_with("GET /abort"));
        assert!(session.abort_held(held.id));
    };
    let ((status, _, body), ()) = tokio::join!(request, abort);
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, "Zentop breakpoint aborted this response");
    let flow = complete(&session, 0).await;
    assert_eq!(
        flow.metadata.state,
        FlowState::Failed("Response aborted at a breakpoint")
    );
    assert!(flow.rules[0].note.contains("aborted"));

    // A hold that times out continues unmodified.
    let started = std::time::Instant::now();
    let (status, _, body) = send(
        &session,
        "POST",
        &format!("http://{address}/slow"),
        &[],
        "kept",
    )
    .await;
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "POST /slow x-test=- body=kept");
    let flow = complete(&session, 1).await;
    assert!(flow.rules[0].note.contains("timed out"), "{:?}", flow.rules);
    assert_eq!(session.counters().holds_timed_out, 1);

    // Pausing releases held messages unmodified.
    session.set_rules(
        RuleSet::new(
            vec![rule(
                2,
                &format!("http://{address}/slow"),
                Action::Breakpoint {
                    request: true,
                    response: false,
                },
            )],
            true,
        )
        .unwrap(),
    );
    let slow_url = format!("http://{address}/slow");
    let request = send(&session, "POST", &slow_url, &[], "go");
    let pause = async {
        wait(|| session.held_count() == 1).await;
        assert!(session.pause(true));
    };
    let ((status, _, body), ()) = tokio::join!(request, pause);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "POST /slow x-test=- body=go");
    let flow = complete(&session, 2).await;
    assert!(flow.rules[0].note.contains("released when capture paused"));
    origin.abort();
}

#[tokio::test]
async fn breakpoints_switch_off_and_cap_held_messages() {
    let (address, origin) = echo_origin().await;
    let session = proxy(true);
    let hold = |enabled| {
        RuleSet::new(
            vec![rule(
                1,
                &format!("http://{address}/*"),
                Action::Breakpoint {
                    request: true,
                    response: false,
                },
            )],
            enabled,
        )
        .unwrap()
    };
    session.set_rules(hold(true));
    let mut pending = Vec::new();
    for index in 0..zentop_proxy::rules::HOLD_LIMIT {
        let url = format!("http://{address}/{index}");
        let address = session.address();
        let auth = auth(&session);
        pending.push(tokio::spawn(async move {
            let (mut sender, _driver) = client(address).await;
            let response = sender
                .send_request(
                    Request::get(url)
                        .header("proxy-authorization", auth)
                        .body(Full::default())
                        .unwrap(),
                )
                .await
                .unwrap();
            response.status()
        }));
    }
    wait(|| session.held_count() == zentop_proxy::rules::HOLD_LIMIT).await;
    // Beyond the cap, matching requests pass through and are counted.
    let (status, _, _) = send(&session, "GET", &format!("http://{address}/extra"), &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session.counters().holds_skipped, 1);
    // Turning the master switch off releases everything.
    session.set_rules(hold(false));
    for request in pending {
        assert_eq!(request.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(session.held_count(), 0);
    let (status, _, _) = send(&session, "GET", &format!("http://{address}/after"), &[], "").await;
    assert_eq!(status, StatusCode::OK);
    origin.abort();
}

#[tokio::test]
async fn map_local_serves_files_without_contacting_the_origin() {
    let dir = std::env::temp_dir().join(format!("zentop-map-local-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("users.json");
    std::fs::write(&file, br#"{"users":["local"]}"#).unwrap();
    let session = proxy(false);
    let local = |id, url: &str, path: &std::path::Path| {
        rule(
            id,
            url,
            Action::MapLocal {
                path: path.to_str().unwrap().into(),
                status: 200,
                content_type: None,
                headers: vec![("x-mock".into(), "zentop".into())],
            },
        )
    };
    session.set_rules(
        RuleSet::new(
            vec![
                local(1, "http://api.example.invalid/users*", &file),
                local(
                    2,
                    "http://api.example.invalid/missing",
                    &dir.join("nope.json"),
                ),
                local(3, "http://api.example.invalid/dir", &dir),
                rule(
                    4,
                    "http://api.example.invalid/users",
                    Action::Rewrite {
                        actions: vec![RewriteAction::SetHeader {
                            side: RuleSide::Response,
                            name: "x-rewritten".into(),
                            value: "after map local".into(),
                        }],
                    },
                ),
            ],
            true,
        )
        .unwrap(),
    );
    let (status, headers, body) = send(
        &session,
        "POST",
        "http://api.example.invalid/users?page=1",
        &[],
        "uploaded",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"users":["local"]}"#);
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(headers["content-length"], body.len().to_string());
    assert_eq!(headers["x-mock"], "zentop");
    assert_eq!(headers["x-rewritten"], "after map local");
    let flow = complete(&session, 0).await;
    assert_eq!(flow.metadata.state, FlowState::Complete);
    assert_eq!(flow.request_body.kept(), b"uploaded");
    assert_eq!(flow.response_body.kept(), br#"{"users":["local"]}"#);
    assert_eq!(
        flow.metadata.rules,
        vec![(1, RuleKind::MapLocal), (4, RuleKind::Rewrite)]
    );
    for (id, path, reason) in [
        (1, "missing", "Map Local file not found"),
        (2, "dir", "Map Local path is not a regular file"),
    ] {
        let (status, _, body) = send(
            &session,
            "GET",
            &format!("http://api.example.invalid/{path}"),
            &[],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body, format!("Zentop Map Local: {reason}"));
        let flow = complete(&session, id).await;
        assert_eq!(flow.metadata.state, FlowState::Failed(reason));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn map_remote_follows_destination_policy() {
    let (first, first_origin) = echo_origin().await;
    let (second, second_origin) = echo_origin().await;
    let remote = |url: String, port| {
        RuleSet::new(
            vec![rule(
                1,
                &url,
                Action::MapRemote {
                    scheme: None,
                    host: Some("127.0.0.1".into()),
                    port: Some(port),
                    path: Some("/v2".into()),
                    keep_query: true,
                },
            )],
            true,
        )
        .unwrap()
    };
    let session = proxy(true);
    session.set_rules(remote(format!("http://{first}/v1/*"), second.port()));
    let (status, headers, body) = send(
        &session,
        "GET",
        &format!("http://{first}/v1/users?page=2"),
        &[],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-origin"], "echo");
    assert_eq!(body, "GET /v2?page=2 x-test=- body=");
    let flow = complete(&session, 0).await;
    assert_eq!(flow.metadata.host, second.to_string());
    assert_eq!(flow.rules[0].kind, RuleKind::MapRemote);
    assert_eq!(flow.rules[0].note, format!("Sent to http://{second}"));

    // A local destination still needs Allow local destinations.
    let blocked = proxy(false);
    blocked.set_rules(remote("http://example.invalid/*".into(), second.port()));
    let (status, _, _) = send(&blocked, "GET", "http://example.invalid/a", &[], "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let flow = complete(&blocked, 0).await;
    assert!(matches!(flow.metadata.state, FlowState::Refused(_)));
    assert_eq!(blocked.counters().refused, 1);
    first_origin.abort();
    second_origin.abort();
}

/// A one-connection HTTPS origin for `localhost` with a self-signed certificate.
async fn tls_origin() -> (u16, rustls::pki_types::CertificateDer<'static>) {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let der = certificate.cert.der().clone();
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![der.clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
    )
    .unwrap();
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tls = tls.clone();
            tokio::spawn(async move {
                let Ok(stream) = TlsAcceptor::from(Arc::new(tls)).accept(stream).await else {
                    return;
                };
                let service = service_fn(|request: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "secure {} {:?}",
                        request.uri().path(),
                        request.version()
                    )))))
                });
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (port, der)
}

#[tokio::test]
async fn map_remote_to_https_verifies_the_upstream_certificate() {
    let (port, certificate) = tls_origin().await;
    for trusted in [true, false] {
        let verifier = tls_client(trusted.then(|| certificate.clone()), b"h2");
        let session = Session::start_with_decryption(
            Options {
                allow_local_destinations: true,
                ..Options::default()
            },
            Arc::new(zentop_proxy::Decryption::with_client_config(verifier)),
        )
        .unwrap();
        session.set_rules(
            RuleSet::new(
                vec![rule(
                    1,
                    "http://plain.example.invalid/*",
                    Action::MapRemote {
                        scheme: Some("https".into()),
                        host: Some("localhost".into()),
                        port: Some(port),
                        path: None,
                        keep_query: true,
                    },
                )],
                true,
            )
            .unwrap(),
        );
        let (status, _, body) =
            send(&session, "GET", "http://plain.example.invalid/doc", &[], "").await;
        let flow = complete(&session, 0).await;
        if trusted {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, "secure /doc HTTP/2.0");
            assert!(flow.metadata.timing.tls.is_some());
            assert_eq!(flow.request.url, format!("https://localhost:{port}/doc"));
        } else {
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert!(matches!(flow.metadata.state, FlowState::UpstreamTls(_)));
        }
    }
}

#[tokio::test]
async fn rewrites_headers_status_query_and_bodies() {
    let (address, origin) = echo_origin().await;
    let session = proxy(true);
    session.set_rules(
        RuleSet::new(
            vec![rule(
                1,
                &format!("http://{address}/*"),
                Action::Rewrite {
                    actions: vec![
                        RewriteAction::SetHeader {
                            side: RuleSide::Request,
                            name: "x-test".into(),
                            value: "rewritten".into(),
                        },
                        RewriteAction::SetQuery {
                            name: "page".into(),
                            value: "2".into(),
                        },
                        RewriteAction::RemoveQuery {
                            name: "debug".into(),
                        },
                        RewriteAction::ReplaceBody {
                            side: RuleSide::Request,
                            find: "free".into(),
                            replace: "premium".into(),
                            regex: false,
                        },
                        RewriteAction::Status { code: 201 },
                        RewriteAction::RemoveHeader {
                            side: RuleSide::Response,
                            name: "x-origin".into(),
                        },
                        RewriteAction::AddHeader {
                            side: RuleSide::Response,
                            name: "x-zentop".into(),
                            value: "rewrite".into(),
                        },
                        RewriteAction::ReplaceBody {
                            side: RuleSide::Response,
                            find: r"body=(\w+)".into(),
                            replace: "BODY<$1>".into(),
                            regex: true,
                        },
                    ],
                },
            )],
            true,
        )
        .unwrap(),
    );
    for (path, encoded) in [("/plan", false), ("/plan/gzip", true)] {
        let (status, headers, body) = send(
            &session,
            "POST",
            &format!("http://{address}{path}?page=1&debug=1"),
            &[("x-test", "original"), ("content-type", "text/plain")],
            "plan=free",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            body,
            format!("POST {path}?page=2 x-test=rewritten BODY<plan>=premium")
        );
        assert_eq!(headers["content-length"], body.len().to_string());
        assert!(!headers.contains_key("x-origin"));
        assert_eq!(headers["x-zentop"], "rewrite");
        // Compressed bodies are rewritten decoded and sent uncompressed.
        assert!(!headers.contains_key("content-encoding"), "{encoded}");
    }
    let flow = complete(&session, 0).await;
    assert_eq!(flow.metadata.status, Some(201));
    assert_eq!(flow.request_body.kept(), b"plan=premium");
    assert!(flow.request.url.ends_with("/plan?page=2"));
    assert_eq!(flow.metadata.rules, vec![(1, RuleKind::Rewrite)]);
    assert!(flow.rules[0].note.contains("Response body rewritten"));
    origin.abort();
}

#[tokio::test]
async fn rules_never_touch_tunnels() {
    let (address, origin) = echo_origin().await;
    let session = proxy(true);
    session.set_rules(
        RuleSet::new(
            vec![
                rule(
                    1,
                    "*",
                    Action::Breakpoint {
                        request: true,
                        response: true,
                    },
                ),
                rule(
                    2,
                    "*",
                    Action::Rewrite {
                        actions: vec![RewriteAction::AddHeader {
                            side: RuleSide::Response,
                            name: "x-zentop".into(),
                            value: "1".into(),
                        }],
                    },
                ),
            ],
            true,
        )
        .unwrap(),
    );
    let (mut sender, _driver) = client(session.address()).await;
    let response = sender
        .send_request(
            Request::builder()
                .method("CONNECT")
                .uri(address.to_string())
                .header("proxy-authorization", auth(&session))
                .body(Full::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let tunnel = hyper::upgrade::on(response).await.unwrap();
    let (mut inner, connection) = hyper::client::conn::http1::handshake(tunnel).await.unwrap();
    tokio::spawn(connection);
    let response = inner
        .send_request(
            Request::get("/through")
                .header("host", address.to_string())
                .body(Full::<Bytes>::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!response.headers().contains_key("x-zentop"));
    assert_eq!(
        response.collect().await.unwrap().to_bytes(),
        "GET /through x-test=- body="
    );
    assert_eq!(session.held_count(), 0);
    let flow = session.flow(0, false).unwrap();
    assert_eq!(flow.metadata.kind, FlowKind::Tunnel);
    assert!(flow.rules.is_empty() && flow.metadata.rules.is_empty());
    origin.abort();
}

#[tokio::test]
async fn rewrites_apply_inside_decrypted_connections() {
    let test = DecryptedTest::new(false, true, 1).await;
    test.session.set_rules(
        RuleSet::new(
            vec![rule(
                1,
                &format!("https://localhost:{}/capture", test.port),
                Action::Rewrite {
                    actions: vec![RewriteAction::SetHeader {
                        side: RuleSide::Response,
                        name: "x-origin".into(),
                        value: "rewritten inside TLS".into(),
                    }],
                },
            )],
            true,
        )
        .unwrap(),
    );
    let tls = test.connect("localhost", false, true).await.unwrap();
    let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(driver);
    let response = sender.send_request(test.post(false)).await.unwrap();
    assert_eq!(response.headers()["x-origin"], "rewritten inside TLS");
    response.collect().await.unwrap();
    let flow = complete(&test.session, 1).await;
    assert_eq!(flow.metadata.kind, FlowKind::DecryptedHttps);
    assert_eq!(flow.metadata.rules, vec![(1, RuleKind::Rewrite)]);
}
