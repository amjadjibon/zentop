use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use zentop_proxy::{FlowKind, Headers, Metadata, Options, Session};

#[derive(Serialize)]
struct Entry {
    #[serde(flatten)]
    flow: Metadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_headers: Option<Headers>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_headers: Option<Headers>,
}

pub fn run(options: Options, json: bool, headers: bool, output: &mut impl Write) -> io::Result<()> {
    let stopping = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, stopping.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stopping.clone())?;
    let session = Session::start(options)?;
    eprintln!(
        "Listening on {}\nSession token: {}\nexport http_proxy={}\nexport https_proxy=$http_proxy",
        session.address(),
        session.token(),
        session.proxy_url()
    );
    loop {
        for (flow, captured) in session.take_finished(headers) {
            let (request_headers, response_headers) = captured.unzip();
            let entry = Entry {
                flow,
                request_headers,
                response_headers,
            };
            if json {
                serde_json::to_writer(&mut *output, &entry)?;
                writeln!(output)?;
            } else {
                print_entry(&entry, output)?;
            }
        }
        output.flush()?;
        if stopping.load(Ordering::Relaxed) {
            let counters = session.counters();
            eprintln!(
                "Refused {} · Flows dropped {} · Unattributed flows {} · Bodies evicted {}",
                counters.refused,
                counters.flows_dropped,
                counters.unattributed,
                counters.evicted_bodies,
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn print_entry(entry: &Entry, output: &mut impl Write) -> io::Result<()> {
    let flow = &entry.flow;
    // Debug formatting prevents terminal escape sequences in process names/headers.
    write!(
        output,
        "{} {}{}  {}  {} B sent / {} B received  {:.1} ms  {:?}",
        flow.method,
        flow.host,
        flow.path,
        flow.status
            .map_or_else(|| "—".to_string(), |status| status.to_string()),
        flow.bytes.0,
        flow.bytes.1,
        flow.timing.total.as_secs_f64() * 1_000.,
        flow.process.name(),
    )?;
    match flow.kind {
        FlowKind::Tunnel => write!(output, "  Not decrypted")?,
        FlowKind::Upgrade => write!(output, "  Upgraded connection")?,
        _ => {}
    }
    if let Some(reason) = flow.state.error_reason() {
        write!(output, "  {reason}")?;
    }
    writeln!(output)?;
    for (label, headers) in [
        ("Request", &entry.request_headers),
        ("Response", &entry.response_headers),
    ] {
        if let Some(headers) = headers {
            for (name, value) in headers {
                writeln!(output, "  {label} {name}: {value:?}")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::time::SystemTime;
    use zentop_proxy::{Attribution, FlowState, Timing};

    #[test]
    fn traffic_accepts_allow_local_and_json() {
        let args =
            crate::Args::try_parse_from(["zentop-cli", "traffic", "--allow-local", "--json"])
                .unwrap();
        assert!(args.json);
        assert!(matches!(
            args.command,
            Some(crate::Command::Traffic {
                allow_local: true,
                ..
            })
        ));
        let defaults = crate::Args::try_parse_from(["zentop-cli", "traffic"]).unwrap();
        assert!(matches!(
            defaults.command,
            Some(crate::Command::Traffic {
                allow_local: false,
                ..
            })
        ));
    }

    #[test]
    fn json_includes_kind_state_and_millisecond_timings() {
        let entry = Entry {
            flow: Metadata {
                id: 0,
                connection: 0,
                process: Attribution::Unknown("test"),
                kind: FlowKind::Upgrade,
                repeated: false,
                method: "GET".into(),
                host: "localhost:8000".into(),
                path: "/socket".into(),
                started: SystemTime::UNIX_EPOCH,
                status: Some(101),
                timing: Timing {
                    attribution: Some(Duration::from_micros(1_250)),
                    connect: Some(Duration::from_micros(14_800)),
                    tls: None,
                    ttfb: Some(Duration::from_millis(15)),
                    total: Duration::from_millis(40),
                },
                bytes: (5, 5),
                state: FlowState::Complete,
            },
            request_headers: None,
            response_headers: None,
        };
        let json = serde_json::to_value(entry).unwrap();
        assert_eq!(json["kind"], "upgrade");
        assert_eq!(json["state"]["kind"], "complete");
        assert_eq!(
            json["timing"],
            serde_json::json!({
                "attribution_ms": 1.25, "connect_ms": 14.8, "tls_ms": null,
                "ttfb_ms": 15.0, "total_ms": 40.0
            })
        );
    }
}
