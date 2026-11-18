//! On-demand, bounded TCP/UDP snapshots. No DNS lookups or packet capture.
use serde::Serialize;
use std::{collections::BTreeSet, net::SocketAddr};

pub const MAX_CONNECTIONS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Protocol {
    Tcp,
    Udp,
}
impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Connection {
    pub protocol: Protocol,
    pub local: SocketAddr,
    /// None for TCP listeners and UDP (whose peer is not exposed by the backend).
    pub remote: Option<SocketAddr>,
    pub state: String,
}
impl Connection {
    pub fn remote_label(&self) -> String {
        self.remote.map_or_else(
            || {
                if self.state == "LISTEN" {
                    "—".into()
                } else {
                    "Unavailable".into()
                }
            },
            |a| a.to_string(),
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub pid: u32,
    pub start_time: u64,
    pub captured_at: u64,
    pub connections: Vec<Connection>,
    pub truncated: bool,
    pub partial: bool,
}

/// Identity accompanies errors as well as successful reports, so a queued result
/// cannot be displayed in a newly opened inspector for a different process.
#[derive(Clone, Debug)]
pub struct Observation {
    pub identity: crate::process_view::Identity,
    pub result: Result<Report, String>,
}

/// Refresh only one process; do not read command lines or environments here.
pub fn process_start_time(pid: u32) -> Result<u64, String> {
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .map(|p| p.start_time())
        .ok_or_else(|| "Process exited or its identity cannot be read.".into())
}

pub fn collect(pid: u32, start_time: u64) -> Result<Report, String> {
    check_identity(pid, start_time)?;
    let mut report = collect_sockets(pid, start_time)?;
    check_identity(pid, start_time)?;
    report.captured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    Ok(report)
}
fn check_identity(pid: u32, start_time: u64) -> Result<(), String> {
    if process_start_time(pid)? == start_time {
        Ok(())
    } else {
        Err("Process identity changed; reopen its inspector.".into())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn collect_sockets(pid: u32, start_time: u64) -> Result<Report, String> {
    use netstat2::{AddressFamilyFlags as Af, ProtocolFlags as Pf, ProtocolSocketInfo as Info};
    let sockets = netstat2::iterate_sockets_info(Af::IPV4 | Af::IPV6, Pf::TCP | Pf::UDP)
        .map_err(|e| format!("Connections unavailable: {e}"))?;
    let mut rows = BTreeSet::new();
    let (mut truncated, mut partial) = (false, false);
    for socket in sockets {
        let socket = match socket {
            Ok(s) => s,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        if !socket.associated_pids.contains(&pid) {
            continue;
        }
        let connection = match socket.protocol_socket_info {
            Info::Tcp(tcp) => Connection {
                protocol: Protocol::Tcp,
                local: SocketAddr::new(tcp.local_addr, tcp.local_port),
                remote: (tcp.remote_port != 0 && tcp.state != netstat2::TcpState::Listen)
                    .then(|| SocketAddr::new(tcp.remote_addr, tcp.remote_port)),
                state: tcp.state.to_string(),
            },
            Info::Udp(udp) => Connection {
                protocol: Protocol::Udp,
                local: SocketAddr::new(udp.local_addr, udp.local_port),
                remote: None,
                state: "BOUND".into(),
            },
        };
        if connection.local.port() == 0 {
            continue;
        }
        retain(&mut rows, connection, &mut truncated);
    }
    Ok(Report {
        pid,
        start_time,
        captured_at: 0,
        connections: rows.into_iter().collect(),
        truncated,
        partial,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn collect_sockets(_: u32, _: u64) -> Result<Report, String> {
    Err("Connection inspection is unavailable on this operating system.".into())
}
fn retain(rows: &mut BTreeSet<Connection>, connection: Connection, truncated: &mut bool) {
    rows.insert(connection);
    if rows.len() > MAX_CONNECTIONS {
        rows.pop_last();
        *truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream, UdpSocket};

    #[test]
    fn live_loopback_reports_listener_both_tcp_endpoints_and_bound_udp() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let pid = std::process::id();
        let report = collect(pid, process_start_time(pid).unwrap()).unwrap();
        assert!(
            report
                .connections
                .iter()
                .any(|c| c.local == listener.local_addr().unwrap()
                    && c.state == "LISTEN"
                    && c.remote.is_none()),
            "{report:?}"
        );
        for socket in [&client, &server] {
            assert!(
                report
                    .connections
                    .iter()
                    .any(|c| c.local == socket.local_addr().unwrap()
                        && c.remote == Some(socket.peer_addr().unwrap())
                        && c.state == "ESTABLISHED"),
                "{report:?}"
            );
        }
        assert!(
            report
                .connections
                .iter()
                .any(|c| c.local == udp.local_addr().unwrap()
                    && c.protocol == Protocol::Udp
                    && c.remote.is_none()),
            "{report:?}"
        );
    }
    #[test]
    fn ipv6_loopback_preserves_addresses_and_remote_ports() {
        let Ok(listener) = TcpListener::bind("[::1]:0") else {
            return;
        };
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let pid = std::process::id();
        let report = collect(pid, process_start_time(pid).unwrap()).unwrap();
        assert!(
            report
                .connections
                .iter()
                .any(|c| c.local == client.local_addr().unwrap()
                    && c.remote == Some(client.peer_addr().unwrap())),
            "{report:?}"
        );
    }
    #[test]
    fn refuses_a_reused_process_identity() {
        let pid = std::process::id();
        assert!(
            collect(pid, process_start_time(pid).unwrap().wrapping_add(1))
                .unwrap_err()
                .contains("identity changed")
        );
    }
    #[test]
    fn rows_are_deduplicated_sorted_and_bounded() {
        let mut rows = BTreeSet::new();
        let mut truncated = false;
        for port in (1..=1000).rev() {
            let c = Connection {
                protocol: Protocol::Tcp,
                local: format!("127.0.0.1:{port}").parse().unwrap(),
                remote: None,
                state: "LISTEN".into(),
            };
            retain(&mut rows, c.clone(), &mut truncated);
            retain(&mut rows, c, &mut truncated);
        }
        assert_eq!(rows.len(), MAX_CONNECTIONS);
        assert!(truncated);
        assert_eq!(rows.first().unwrap().local.port(), 1);
        assert_eq!(rows.last().unwrap().local.port(), MAX_CONNECTIONS as u16);
        assert_eq!(
            Connection {
                protocol: Protocol::Tcp,
                local: "[::1]:443".parse().unwrap(),
                remote: Some("[2001:db8::1]:5000".parse().unwrap()),
                state: "ESTABLISHED".into()
            }
            .remote_label(),
            "[2001:db8::1]:5000"
        );
    }
}
