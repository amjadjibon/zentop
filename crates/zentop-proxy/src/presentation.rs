use std::fmt::Write as _;
use std::io::{self, Read};
use std::time::SystemTime;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;

use crate::{Attribution, Body, ContentKind, Flow, FlowKind, FlowState, Headers, Timing};

#[derive(Clone, Debug, Serialize)]
pub struct Metadata {
    pub id: u64,
    pub connection: u64,
    pub started: SystemTime,
    pub process: Attribution,
    pub kind: FlowKind,
    pub repeated: bool,
    pub method: String,
    pub host: String,
    /// Paths here exclude queries, including when used by the CLI.
    pub path: String,
    pub status: Option<u16>,
    pub timing: Timing,
    pub bytes: (u64, u64),
    pub state: FlowState,
    /// The request URL with secret query values masked. The CLI omits it,
    /// as it omits queries everywhere.
    #[serde(skip)]
    pub url: String,
    /// Classified by the store as heads and the request body arrive.
    #[serde(skip)]
    pub content: ContentKind,
    #[serde(skip)]
    pub websocket: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum Secrets {
    Show,
    Mask,
    Omit,
}

pub(crate) fn secret_header(name: &str) -> bool {
    // WebSocket handshake values are per-connection nonces, not credentials,
    // even though "sec-websocket-key" contains the word "key".
    if name.starts_with("sec-websocket-") {
        return false;
    }
    matches!(name, "authorization" | "cookie" | "set-cookie") || secret_key(name)
}

/// Replaces a secret value wherever the shared redaction policy masks it.
pub const MASK: &str = "[redacted]";

pub(crate) fn headers(raw: &Headers, secrets: Secrets) -> Headers {
    raw.iter()
        .filter_map(|(name, value)| {
            if name == "proxy-authorization" {
                return None;
            }
            if secret_header(name) {
                return match secrets {
                    Secrets::Show => Some((name.clone(), value.clone())),
                    Secrets::Mask => Some((name.clone(), MASK.into())),
                    Secrets::Omit => None,
                };
            }
            Some((name.clone(), value.clone()))
        })
        .collect()
}

pub(crate) fn secret_key(name: &str) -> bool {
    let name = name.as_bytes();
    let mut decoded = Vec::with_capacity(name.len());
    let mut i = 0;
    while i < name.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        if name[i] == b'%' && i + 2 < name.len() {
            if let (Some(high), Some(low)) = (hex(name[i + 1]), hex(name[i + 2])) {
                decoded.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(name[i]);
        i += 1;
    }
    decoded.make_ascii_lowercase();
    const KEYS: &[&[u8]] = &[
        b"token",
        b"key",
        b"secret",
        b"password",
        b"sig",
        b"code",
        b"auth",
    ];
    decoded
        .split(|b| !b.is_ascii_alphanumeric())
        .any(|word| KEYS.contains(&word))
        || [
            b"apikey".as_slice(),
            b"accesstoken",
            b"clientsecret",
            b"refreshtoken",
        ]
        .contains(&decoded.as_slice())
}

pub(crate) fn url(raw: &str, secrets: Secrets) -> String {
    let Some((base, query)) = raw.split_once('?') else {
        return raw.to_owned();
    };
    if matches!(secrets, Secrets::Show) {
        return raw.to_owned();
    }
    let mut display = base.to_owned();
    let mut sep = '?';
    for pair in query.split('&') {
        let (name, _) = pair.split_once('=').unwrap_or((pair, ""));
        let secret = secret_key(name);
        if secret && matches!(secrets, Secrets::Omit) {
            continue;
        }
        display.push(sep);
        sep = '&';
        if secret {
            display.push_str(name);
            display.push('=');
            display.push_str(MASK);
        } else {
            display.push_str(pair);
        }
    }
    display
}

fn shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn copy_as_curl(flow: &Flow, include_secrets: bool) -> Option<String> {
    if !matches!(
        flow.metadata.kind,
        FlowKind::Http | FlowKind::DecryptedHttps
    ) {
        return None;
    }
    let secrets = if include_secrets {
        Secrets::Show
    } else {
        Secrets::Omit
    };
    let mut command = format!(
        "curl --request {} --url {}",
        shell(&flow.request.method),
        shell(&url(&flow.request.url, secrets))
    );
    for (name, value) in headers(&flow.request.headers, secrets) {
        // curl regenerates framing when replaying a captured body.
        if !matches!(
            name.as_str(),
            "content-length"
                | "transfer-encoding"
                | "host"
                | "connection"
                | "proxy-connection"
                | "keep-alive"
                | "upgrade"
                | "te"
                | "trailer"
        ) {
            let _ = write!(command, " --header {}", shell(&format!("{name}: {value}")));
        }
    }
    if !flow.request_body.kept().is_empty() {
        if let Ok(body) = std::str::from_utf8(flow.request_body.kept()) {
            if !body.contains('\0') {
                let _ = write!(command, " --data-binary {}", shell(body));
                return Some(command);
            }
        }
        let mut octal = String::new();
        for byte in flow.request_body.kept() {
            let _ = write!(octal, "\\{byte:03o}");
        }
        command = format!("printf {} | {command} --data-binary @-", shell(&octal));
    }
    Some(command)
}

/// Returns the retained wire body as text when possible, or as base64.
pub fn copy_body(body: &Body) -> Option<(&'static str, String)> {
    let bytes = body.kept();
    if bytes.is_empty() {
        return None;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        if is_text(text) {
            return Some(("body", text.to_owned()));
        }
    }
    Some(("body (base64)", STANDARD.encode(bytes)))
}

fn is_text(text: &str) -> bool {
    !text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n'))
}

const PREVIEW_LIMIT: usize = 256 * 1024;

#[derive(Debug)]
pub struct BodyPreview {
    pub text: String,
    pub binary: bool,
    pub limited: bool,
}

pub fn preview(body: &Body, headers: &Headers) -> io::Result<BodyPreview> {
    let header = |wanted| {
        headers
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.as_str())
    };
    let raw = body.kept();
    let mut source: Box<dyn Read + '_> = Box::new(raw);
    if !raw.is_empty() {
        for encoding in header("content-encoding").unwrap_or("identity").rsplit(',') {
            source = match encoding.trim().to_ascii_lowercase().as_str() {
                "identity" => source,
                "gzip" => Box::new(flate2::read::MultiGzDecoder::new(source)),
                "br" => Box::new(brotli::Decompressor::new(source, 4096)),
                "deflate" => Box::new(flate2::read::ZlibDecoder::new(source)),
                _ => return Err(io::Error::other("Unsupported content encoding")),
            };
        }
    }
    let mut bytes = Vec::new();
    let result = source
        .take((PREVIEW_LIMIT + 1) as u64)
        .read_to_end(&mut bytes);
    let limited = bytes.len() > PREVIEW_LIMIT || matches!(body, Body::Truncated { .. });
    if !limited {
        result?;
    } else if bytes.is_empty() {
        return Err(io::Error::other("No decoded preview is available"));
    }
    bytes.truncate(PREVIEW_LIMIT);
    let utf8 = match std::str::from_utf8(&bytes) {
        Err(error) if limited && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        }
        result => result.ok(),
    };
    if let Some(text) = utf8.filter(|text| is_text(text)) {
        let mut display = text.to_owned();
        let mut limited = limited;
        if header("content-type").is_some_and(|kind| kind.contains("json")) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
                if let Ok(pretty) = serde_json::to_string_pretty(&value) {
                    let end = pretty.floor_char_boundary(PREVIEW_LIMIT.min(pretty.len()));
                    limited |= end < pretty.len();
                    display = pretty[..end].to_owned();
                }
            }
        }
        return Ok(BodyPreview {
            text: display,
            binary: false,
            limited,
        });
    }
    let mut text = String::new();
    for (offset, line) in bytes.chunks(16).take(PREVIEW_LIMIT / 60).enumerate() {
        let _ = write!(text, "{:06x} ", offset * 16);
        for byte in line {
            let _ = write!(text, " {byte:02x}");
        }
        text.push('\n');
    }
    Ok(BodyPreview {
        text,
        binary: true,
        limited: limited || bytes.len() > PREVIEW_LIMIT / 60 * 16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_compressed_json_and_caps_decompressed_text() {
        use std::io::Write as _;

        let json = br#"{"a":1,"b":2}"#;
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), Default::default());
        gzip.write_all(json).unwrap();
        let mut deflate = flate2::write::ZlibEncoder::new(Vec::new(), Default::default());
        deflate.write_all(json).unwrap();
        let mut br = brotli::CompressorWriter::new(Vec::new(), 4096, 4, 20);
        br.write_all(json).unwrap();
        for (encoding, bytes) in [
            ("gzip", gzip.finish().unwrap()),
            ("deflate", deflate.finish().unwrap()),
            ("br", br.into_inner()),
        ] {
            let headers = vec![
                ("content-encoding".into(), encoding.into()),
                ("content-type".into(), "application/json".into()),
            ];
            let body = preview(&Body::Captured(bytes.into()), &headers).unwrap();
            assert_eq!(body.text, "{\n  \"a\": 1,\n  \"b\": 2\n}");
            assert!(!body.binary && !body.limited);
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), Default::default());
        gzip.write_all(&vec![b'a'; PREVIEW_LIMIT + 100]).unwrap();
        let body = preview(
            &Body::Captured(gzip.finish().unwrap().into()),
            &vec![("content-encoding".into(), "gzip".into())],
        )
        .unwrap();
        assert!(body.limited);
        assert_eq!(body.text.len(), PREVIEW_LIMIT);
    }

    #[test]
    fn masks_or_omits_secret_looking_headers() {
        for name in [
            "x-api-key",
            "api-key",
            "x-auth-token",
            "x-access-token",
            "x-csrf-token",
        ] {
            let raw = vec![
                (name.into(), "private".into()),
                ("x-tokenizer".into(), "public".into()),
            ];
            let masked = headers(&raw, Secrets::Mask);
            assert_eq!(masked[0].1, MASK);
            assert_eq!(masked[1], raw[1]);
            assert_eq!(headers(&raw, Secrets::Omit), raw[1..]);
            assert_eq!(headers(&raw, Secrets::Show), raw);
        }
    }

    #[test]
    fn masks_and_omits_secret_values_including_encoded_names() {
        let raw = "http://example.com/?API_KEY=one&%74oken=two&name=three";
        assert_eq!(
            url(raw, Secrets::Mask),
            "http://example.com/?API_KEY=[redacted]&%74oken=[redacted]&name=three"
        );
        assert_eq!(url(raw, Secrets::Omit), "http://example.com/?name=three");
        assert_eq!(url(raw, Secrets::Show), raw);
        let raw = vec![
            ("authorization".into(), "secret".into()),
            ("proxy-authorization".into(), "token".into()),
        ];
        assert_eq!(
            headers(&raw, Secrets::Mask),
            vec![("authorization".into(), "[redacted]".into())]
        );
        assert!(headers(&raw, Secrets::Omit).is_empty());
        assert_eq!(headers(&raw, Secrets::Show).len(), 1);
    }

    #[test]
    fn websocket_handshake_headers_are_not_secrets() {
        assert!(!secret_header("sec-websocket-key"));
        assert!(!secret_header("sec-websocket-accept"));
        assert!(secret_header("x-api-key"));
        assert!(secret_header("authorization"));
    }
}
