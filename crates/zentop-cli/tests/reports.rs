use std::process::Command;

#[test]
fn process_report_is_bounded_sorted_and_excludes_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_zentop-cli"))
        .args(["processes", "--json", "--sort", "memory", "--limit", "3"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!rows.is_empty() && rows.len() <= 3);
    assert!(rows
        .windows(2)
        .all(|pair| pair[0]["memory_bytes"].as_u64().unwrap()
            >= pair[1]["memory_bytes"].as_u64().unwrap()));
    for row in rows {
        let fields = row.as_object().unwrap();
        assert_eq!(fields.len(), 5);
        for name in ["pid", "name", "cpu_percent", "memory_bytes", "status"] {
            assert!(fields.contains_key(name));
        }
    }
}

#[test]
fn temperature_json_has_named_nullable_celsius_readings() {
    let output = Command::new(env!("CARGO_BIN_EXE_zentop-cli"))
        .args(["temperature", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sensors: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!sensors.is_empty() && sensors.len() <= 131);
    for sensor in sensors {
        assert!(sensor["name"].is_string());
        assert!(sensor["source"].is_string());
        assert!(
            sensor["celsius"].is_null()
                || sensor["celsius"]
                    .as_f64()
                    .is_some_and(|v| (-40.0..=150.0).contains(&v))
        );
    }
}

#[test]
fn connections_json_reports_live_endpoints_without_process_secrets() {
    use std::net::{TcpListener, TcpStream, UdpSocket};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_server, _) = listener.accept().unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_zentop-cli"))
        .args(["connections", &std::process::id().to_string(), "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["pid"], std::process::id());
    let rows = report["connections"].as_array().unwrap();
    assert!(rows.len() <= 512);
    assert!(rows
        .iter()
        .any(|r| r["local"] == client.local_addr().unwrap().to_string()
            && r["remote"] == client.peer_addr().unwrap().to_string()
            && r["state"] == "ESTABLISHED"));
    assert!(rows
        .iter()
        .any(|r| r["local"] == udp.local_addr().unwrap().to_string()
            && r["remote"].is_null()
            && r["protocol"] == "UDP"));
    assert!(!report.to_string().contains("environ"));
}
