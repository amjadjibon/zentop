use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::{self, Instant};

use crate::{store::Store, Attribution, PENDING_ATTRIBUTION};

const MIN_INTERVAL: Duration = Duration::from_millis(25);
const BATCH_LIMIT: usize = 256;

pub(crate) fn resolve(
    store: &mut Store,
    id: u64,
    sender: &watch::Sender<Attribution>,
    owner: Attribution,
) {
    if *sender.borrow() == PENDING_ATTRIBUTION {
        sender.send_replace(owner.clone());
        store.attribute(id, owner);
    }
}

pub(crate) async fn worker(
    port: u16,
    mut pending: mpsc::Receiver<(u64, SocketAddr, watch::Sender<Attribution>)>,
    store: Arc<Mutex<Store>>,
) {
    let mut last_scan = None;
    while let Some(first) = pending.recv().await {
        if let Some(started) = last_scan {
            time::sleep_until(started + MIN_INTERVAL).await;
        }
        let mut batch = vec![first];
        while batch.len() < BATCH_LIMIT {
            let Ok(next) = pending.try_recv() else { break };
            batch.push(next);
        }
        let live: Vec<_> = batch
            .iter()
            .map(|(_, _, sender)| !sender.is_closed() && *sender.borrow() == PENDING_ATTRIBUTION)
            .collect();
        let peers: Vec<_> = batch
            .iter()
            .zip(&live)
            .filter(|(_, live)| **live)
            .map(|((_, peer, _), _)| *peer)
            .collect();
        let paused = store.lock().unwrap().counters.paused;
        let mut owners = Vec::new().into_iter();
        if !paused && !peers.is_empty() {
            let dispatched = Instant::now();
            last_scan = Some(dispatched);
            if let Ok((started, scanned)) =
                tokio::task::spawn_blocking(move || (Instant::now(), scan(port, &peers))).await
            {
                last_scan = Some(started);
                owners = scanned.into_iter();
            }
        }
        let mut store = store.lock().unwrap();
        for ((id, _, sender), live) in batch.into_iter().zip(live) {
            let scanned = live.then(|| owners.next()).flatten();
            let owner = if sender.is_closed() {
                Attribution::unknown("Client disconnected before attribution")
            } else if paused {
                Attribution::unknown("Capture was paused before attribution")
            } else {
                scanned.unwrap_or_else(|| Attribution::unknown("Attribution unavailable"))
            };
            resolve(&mut store, id, &sender, owner);
        }
    }
}

#[cfg(target_os = "macos")]
fn scan(proxy_port: u16, peers: &[SocketAddr]) -> Vec<Attribution> {
    use std::net::IpAddr;

    #[repr(C)]
    struct Peer {
        address: u32,
        port: u16,
    }

    #[derive(Clone, Default)]
    #[repr(C)]
    struct Owner {
        unique_id: u64,
        start_time: u64,
        pid: i32,
        result: i32,
        name: [u8; 32],
    }

    unsafe extern "C" {
        fn zentop_socket_owners(port: u16, peers: *const Peer, owners: *mut Owner, count: usize);
    }

    let peers: Vec<_> = peers
        .iter()
        .map(|peer| Peer {
            address: match peer.ip() {
                IpAddr::V4(address) => u32::from_ne_bytes(address.octets()),
                IpAddr::V6(_) => 0,
            },
            port: peer.port(),
        })
        .collect();
    let mut owners = vec![Owner::default(); peers.len()];
    // The C scanner reads and writes exactly count elements of the two live slices.
    unsafe {
        zentop_socket_owners(proxy_port, peers.as_ptr(), owners.as_mut_ptr(), peers.len());
    }
    owners
        .into_iter()
        .map(|owner| match owner.result {
            1 => Attribution::Known {
                unique_id: owner.unique_id,
                start_time: owner.start_time,
                pid: owner.pid as u32,
                name: String::from_utf8_lossy(
                    owner
                        .name
                        .split(|&byte| byte == 0)
                        .next()
                        .unwrap_or_default(),
                )
                .into_owned(),
            },
            2 => Attribution::unknown("Multiple processes hold the client socket"),
            3 => Attribution::unknown("Client exited or its identity became unavailable"),
            4 => {
                Attribution::unknown("No accessible matching socket; some processes denied access")
            }
            _ => Attribution::unknown("No matching established socket"),
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn scan(_: u16, peers: &[SocketAddr]) -> Vec<Attribution> {
    vec![Attribution::unknown("Process attribution is unavailable on this platform"); peers.len()]
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    // The descriptor inheritance test briefly forks with the test process's
    // other descriptors open. Keep it away from scans in the other tests.
    static SCAN_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn resolves_live_socket_and_leaves_closed_socket_unknown() {
        let _serial = SCAN_TESTS.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (_server, peer) = listener.accept().unwrap();
        let short = TcpStream::connect(addr).unwrap();
        let (_short_server, short_peer) = listener.accept().unwrap();
        drop(short);
        let mismatched = SocketAddr::from(([127, 0, 0, 2], peer.port()));
        let owners = scan(addr.port(), &[peer, short_peer, mismatched]);
        assert!(
            matches!(owners[0], Attribution::Known { pid, unique_id, .. }
            if pid == std::process::id() && unique_id != 0)
        );
        assert!(matches!(owners[1], Attribution::Unknown(_)));
        assert!(matches!(owners[2], Attribution::Unknown(_)));
        drop(client);
    }

    #[test]
    fn shared_client_descriptor_is_ambiguous() {
        let _serial = SCAN_TESTS.lock().unwrap();
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (_server, peer) = listener.accept().unwrap();
        let fd = client.as_raw_fd();
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("10");
        // Only async-signal-safe fcntl runs between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let owners = scan(addr.port(), &[peer]);
        let _ = child.kill();
        child.wait().unwrap();
        assert_eq!(
            owners[0],
            Attribution::Unknown("Multiple processes hold the client socket")
        );
    }

    #[test]
    #[ignore = "measures the macOS socket-table scan"]
    fn benchmark_attribution_scan() {
        let _serial = SCAN_TESTS.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _client = TcpStream::connect(addr).unwrap();
        let (_server, peer) = listener.accept().unwrap();
        let mut durations: Vec<_> = (0..20)
            .map(|_| {
                let started = Instant::now();
                std::hint::black_box(scan(addr.port(), &[peer]));
                started.elapsed()
            })
            .collect();
        durations.sort_unstable();
        eprintln!(
            "20 socket-table scans: median {:?}, min {:?}, max {:?}",
            durations[durations.len() / 2],
            durations[0],
            durations[durations.len() - 1]
        );
    }
}
