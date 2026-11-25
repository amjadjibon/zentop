use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use zentop_proxy::{FlowKind, FlowState, Headers, Metadata, Options, Session};

#[derive(Serialize)]
struct Entry {
    #[serde(flatten)]
    flow: Metadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_headers: Option<Headers>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_headers: Option<Headers>,
}

pub fn run(
    port: Option<u16>,
    json: bool,
    headers: bool,
    output: &mut impl Write,
) -> io::Result<()> {
    let stopping = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, stopping.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stopping.clone())?;
    let session = Session::start(Options {
        port,
        allow_local_destinations: false,
    })?;
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
    if flow.kind == FlowKind::Tunnel {
        write!(output, "  Not decrypted")?;
    }
    if let FlowState::Refused(reason) | FlowState::Failed(reason) = flow.state {
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
