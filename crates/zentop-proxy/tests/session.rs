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
use zentop_proxy::{Attribution, Body, FlowKind, FlowState, Options, Session};

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

#[tokio::test]
async fn forwards_http11_upgrades_and_regular_origin_responses() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
                                let upgrade = hyper::upgrade::on(&mut request);
                                tokio::spawn(async move {
                                    let mut stream = TokioIo::new(upgrade.await.unwrap());
                                    let mut bytes = [0; 5];
                                    stream.read_exact(&mut bytes).await.unwrap();
                                    stream.write_all(&bytes).await.unwrap();
                                });
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
    let mut response = sender.send_request(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(response.headers()["upgrade"], "websocket");
    assert_eq!(response.headers()["connection"], "upgrade");
    let mut stream = TokioIo::new(hyper::upgrade::on(&mut response).await.unwrap());
    stream.write_all(b"hello").await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"hello");
    drop(stream);
    wait(|| session.flow(2, false).unwrap().metadata.state == FlowState::Complete).await;
    let flow = session.flow(2, false).unwrap();
    assert_eq!(flow.metadata.kind, FlowKind::Upgrade);
    assert_eq!(flow.metadata.status, Some(101));
    assert_eq!(flow.metadata.bytes, (5, 5));
    assert!(flow.metadata.timing.attribution.is_some());
    assert!(flow.metadata.timing.connect.is_some());
    assert!(flow.metadata.timing.total >= flow.metadata.timing.ttfb.unwrap());
    for body in [flow.request_body, flow.response_body] {
        assert!(matches!(
            body,
            Body::Skipped("Upgraded connection; frames not captured")
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
        tls.alpn_protocols = vec![if origin_h2 {
            b"h2".to_vec()
        } else {
            b"http/1.1".to_vec()
        }];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(tls) = TlsAcceptor::from(Arc::new(tls)).accept(stream).await else {
                return;
            };
            let server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            let http = service_fn(move |request: Request<hyper::body::Incoming>| async move {
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
            });
            let _ = server.serve_connection(TokioIo::new(tls), http).await;
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
async fn decrypted_connection_enforces_names_and_refuses_upgrades() {
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
                .header("host", format!("localhost:{}", test.port))
                .header("connection", "upgrade")
                .header("upgrade", "websocket"),
            501,
        ),
    ] {
        let response = sender
            .send_request(request.body(Full::<Bytes>::default()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let body = response.collect().await.unwrap().to_bytes();
        if status == 501 {
            assert!(
                String::from_utf8_lossy(&body).contains("remove the host from the decrypt list")
            );
        }
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
