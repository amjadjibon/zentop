//! Process-to-port mapping
//!
//! Collects local bound TCP/UDP ports and groups them by PID. This is more
//! expensive than a sysinfo snapshot, so callers should invoke it only when
//! a port search is active.

use listeners::{Protocol, SocketState};
use std::collections::HashMap;
use std::net::SocketAddr;

/// A port a process accepts connections on (TCP, listening) or has bound (UDP).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalPort {
    pub udp: bool,
    pub address: SocketAddr,
}

/// Every indexed socket with the PID that owns it.
fn local_ports() -> Result<Vec<(u32, LocalPort)>, String> {
    let sockets =
        listeners::get_all().map_err(|e| format!("Local ports could not be read: {e}"))?;
    Ok(sockets
        .into_iter()
        .filter(|l| should_index_socket(l.protocol, l.state, l.socket.port()))
        .map(|l| {
            let port = LocalPort {
                udp: l.protocol == Protocol::UDP,
                address: l.socket,
            };
            (l.process.pid, port)
        })
        .collect())
}

/// Returns local bound port numbers keyed by process ID.
///
/// TCP sockets are included only in LISTEN state. UDP sockets are included
/// when they have a real (non-zero) local port. Collection failures are reported separately so the shared system snapshot remains usable.
pub fn collect_ports_by_pid() -> Result<HashMap<u32, Vec<u16>>, String> {
    let mut ports_by_pid: HashMap<u32, Vec<u16>> = HashMap::new();
    for (pid, port) in local_ports()? {
        ports_by_pid
            .entry(pid)
            .or_default()
            .push(port.address.port());
    }
    for ports in ports_by_pid.values_mut() {
        ports.sort_unstable();
        ports.dedup();
    }
    Ok(ports_by_pid)
}

/// The sockets one process listens on or has bound, sorted TCP first, then by
/// port. Reading them scans every process's sockets, so call it sparingly.
pub fn ports_of(pid: u32) -> Result<Vec<LocalPort>, String> {
    let mut ports: Vec<LocalPort> = local_ports()?
        .into_iter()
        .filter(|&(owner, _)| owner == pid)
        .map(|(_, port)| port)
        .collect();
    ports.sort_unstable_by_key(|p| (p.udp, p.address.port(), p.address.is_ipv6(), p.address.ip()));
    ports.dedup();
    Ok(ports)
}

/// Whether a socket should be searchable from the process search field.
fn should_index_socket(protocol: Protocol, state: SocketState, port: u16) -> bool {
    if port == 0 {
        return false;
    }

    match protocol {
        Protocol::TCP => state == SocketState::Listen,
        Protocol::UDP => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_unspecified_port() {
        assert!(!should_index_socket(Protocol::TCP, SocketState::Listen, 0));
        assert!(!should_index_socket(Protocol::UDP, SocketState::Unknown, 0));
    }

    #[test]
    fn indexes_tcp_listen_only() {
        assert!(should_index_socket(
            Protocol::TCP,
            SocketState::Listen,
            3000
        ));
        assert!(!should_index_socket(
            Protocol::TCP,
            SocketState::Established,
            3000
        ));
    }

    #[test]
    fn finds_this_process_listening_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let ports = ports_of(std::process::id()).unwrap();
        assert!(ports.contains(&LocalPort {
            udp: false,
            address
        }));
    }

    #[test]
    fn indexes_udp_with_real_port() {
        assert!(should_index_socket(Protocol::UDP, SocketState::Unknown, 53));
    }
}
