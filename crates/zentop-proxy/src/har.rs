use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use http::StatusCode;
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{Body, Flow, FlowKind, Headers};

#[derive(Clone, Copy, Debug, Default)]
pub struct HarOptions {
    pub include_bodies: bool,
}

/// Exports a fixed selection one flow at a time. Flows evicted before they are
/// read are skipped. Dropping the session ends access to its capture.
pub struct HarExport {
    pub(crate) store: std::sync::Weak<std::sync::Mutex<crate::store::Store>>,
    pub(crate) ids: Vec<u64>,
}

impl HarExport {
    /// Writes a private file and replaces the destination only after serialization succeeds.
    #[cfg(target_os = "macos")]
    pub fn write_path(self, path: &std::path::Path, options: HarOptions) -> io::Result<usize> {
        let directory = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let mut output = tempfile::NamedTempFile::new_in(directory)?;
        let count = self.write(io::BufWriter::new(output.as_file_mut()), options)?;
        output.persist(path)?;
        Ok(count)
    }

    pub fn write(self, mut writer: impl Write, options: HarOptions) -> io::Result<usize> {
        let mut count = 0;
        writer.write_all(br#"{"log":{"version":"1.2","creator":{"name":"Zentop","version":""#)?;
        writer.write_all(env!("CARGO_PKG_VERSION").as_bytes())?;
        writer.write_all(br#""},"entries":["#)?;
        for id in self.ids {
            let Some(store) = self.store.upgrade() else {
                break;
            };
            let flow = store.lock().unwrap().flow(id, false);
            drop(store);
            if let Some(flow) = flow {
                if count != 0 {
                    writer.write_all(b",")?;
                }
                serde_json::to_writer(&mut writer, &entry(&flow, options))?;
                count += 1;
            }
        }
        writer.write_all(b"]}}")?;
        writer.flush()?;
        Ok(count)
    }
}

fn header_list(headers: &Headers) -> Vec<Value> {
    headers
        .iter()
        .map(|(name, value)| json!({"name":name, "value":value}))
        .collect()
}

fn header<'a>(headers: &'a Headers, name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map_or("", |(_, value)| value.as_str())
}

fn entry(flow: &Flow, options: HarOptions) -> Value {
    let row = &flow.metadata;
    let tunnel = row.kind == FlowKind::Tunnel;
    let handshake = tunnel || row.kind == FlowKind::Upgrade;
    let response_headers = flow.response.as_ref().map(|head| &head.headers);
    let status = row.status.unwrap_or(0);
    let ms = |time: std::time::Duration| time.as_secs_f64() * 1000.;
    let (connect, ssl, blocked) = (row.timing.connect, row.timing.tls, row.timing.attribution);
    let connect_ms = connect.map_or(-1., |tcp| ms(tcp + ssl.unwrap_or_default()));
    let blocked_ms = blocked.map_or(-1., ms);
    let total = if row.state == crate::FlowState::Active {
        row.started.elapsed().map_or(0., ms)
    } else {
        ms(row.timing.total)
    };
    // TTFB includes dialing and attribution. HAR includes SSL within connect.
    let pre_send = connect_ms.max(0.) + blocked_ms.max(0.);
    let wait = row
        .timing
        .ttfb
        .map_or(-1., |elapsed| (ms(elapsed) - pre_send).max(0.));
    let receive = row
        .timing
        .ttfb
        .map_or(0., |elapsed| (total - ms(elapsed)).max(0.));
    let query: Vec<_> = flow
        .request
        .url
        .split_once('?')
        .into_iter()
        .flat_map(|(_, query)| {
            query.split('&').map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                json!({"name": query_part(name), "value": query_part(value)})
            })
        })
        .collect();
    let request_mime = header(&flow.request.headers, "content-type");
    let response_mime = response_headers.map_or("", |headers| header(headers, "content-type"));
    let mut request = json!({
        "method": flow.request.method,
        "url": if tunnel {format!("https://{}/", row.host)} else {flow.request.url.clone()},
        "httpVersion": flow.request.version,
        "cookies": [],
        "headers": header_list(&flow.request.headers),
        "queryString": query,
        "headersSize": -1,
        "bodySize": if handshake {0} else {row.bytes.0},
    });
    let mut content =
        json!({"size": if handshake {0} else {row.bytes.1}, "mimeType": response_mime});
    if options.include_bodies && !handshake {
        if !flow.request_body.kept().is_empty() {
            let mut post = json!({"mimeType":request_mime, "params":[]});
            // HAR postData has no encoding field. Use an extension for binary.
            add_text(&mut post, &flow.request_body, "_encoding");
            request["postData"] = post;
        }
        if !flow.response_body.kept().is_empty() {
            add_text(&mut content, &flow.response_body, "encoding");
        }
    }
    json!({
        "startedDateTime": OffsetDateTime::from(row.started).format(&Rfc3339).unwrap_or_default(),
        "time": pre_send + wait.max(0.) + receive,
        "request": request,
        "response": {
            "status": status,
            "statusText": StatusCode::from_u16(status).ok().and_then(|status| status.canonical_reason()).unwrap_or(""),
            "httpVersion": flow.response.as_ref().map_or("", |head| head.version.as_str()),
            "cookies": [],
            "headers": response_headers.map_or_else(Vec::new, header_list),
            "content": content,
            "redirectURL": response_headers.map_or_else(String::new, |headers| {
                crate::presentation::url(header(headers, "location"), crate::presentation::Secrets::Mask)
            }),
            "headersSize": -1,
            "bodySize": if handshake {0} else {row.bytes.1},
        },
        "cache": {},
        "timings": {
            "blocked": blocked_ms, "dns": -1, "connect": connect_ms, "ssl": ssl.map_or(-1., ms),
            "send": 0, "wait": wait.max(0.), "receive": receive,
        },
        "_zentopRepeated": row.repeated,
    })
}

fn add_text(container: &mut Value, body: &Body, encoding_field: &str) {
    match std::str::from_utf8(body.kept()) {
        Ok(text) => container["text"] = json!(text),
        Err(_) => {
            container["text"] = json!(STANDARD.encode(body.kept()));
            container[encoding_field] = json!("base64");
        }
    }
    if matches!(body, Body::Truncated { .. }) {
        container["_zentopTruncated"] = json!(true);
    }
}

fn query_part(part: &str) -> String {
    let mut bytes = Vec::with_capacity(part.len());
    let mut remaining = part.as_bytes();
    while let [first, rest @ ..] = remaining {
        remaining = rest;
        match (*first, rest) {
            (b'%', [hi, lo, tail @ ..]) => {
                let digit = |b| char::from(b).to_digit(16).map(|n| n as u8);
                if let Some((hi, lo)) = digit(*hi).zip(digit(*lo)) {
                    bytes.push(hi * 16 + lo);
                    remaining = tail;
                } else {
                    bytes.push(*first);
                }
            }
            (b'+', _) => bytes.push(b' '),
            _ => bytes.push(*first),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
