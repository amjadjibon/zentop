use std::mem;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use flate2::{Decompress, FlushDecompress};
use http::HeaderMap;

use crate::store::Store;

pub(crate) const FRAME_LIMIT: usize = 1_000;
pub(crate) const PAYLOAD_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    Text,
    Binary,
    Continuation,
    Ping,
    Pong,
    Close(Option<u16>),
    Other(u8),
}

#[derive(Debug)]
pub struct Message {
    pub direction: Direction,
    pub time: SystemTime,
    pub opcode: Opcode,
    /// Wire payload length of this frame, before decompression.
    pub size: u64,
    pub payload: Vec<u8>,
    /// Data type of the assembled payload, including for a final continuation.
    pub text: bool,
    pub note: Option<&'static str>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Compression {
    client: Option<Window>,
    server: Option<Window>,
}

#[derive(Clone, Copy)]
struct Window {
    reset: bool,
}

impl Compression {
    pub(crate) fn negotiated(request: &HeaderMap, response: &HeaderMap) -> Self {
        const HEADER: &str = "sec-websocket-extensions";
        let deflate = |extension: &str| {
            extension
                .split(';')
                .next()
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("permessage-deflate"))
        };
        let offered = request
            .get_all(HEADER)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(deflate);
        if !offered {
            return Self::default();
        }
        let Some(extension) = response
            .get_all(HEADER)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .find(|v| deflate(v))
        else {
            return Self::default();
        };
        let has = |name: &str| {
            extension
                .split(';')
                .skip(1)
                .any(|parameter| parameter.trim().eq_ignore_ascii_case(name))
        };
        Self {
            client: Some(Window {
                reset: has("client_no_context_takeover"),
            }),
            server: Some(Window {
                reset: has("server_no_context_takeover"),
            }),
        }
    }
}

struct Frame {
    opcode: Opcode,
    finished: bool,
    compressed: bool,
    mask: Option<[u8; 4]>,
    size: u64,
    read: u64,
    time: SystemTime,
    close: [u8; 2],
}

struct Data {
    text: bool,
    compressed: bool,
    bytes: Working,
    skipped: bool,
}

struct Working {
    store: Arc<Mutex<Store>>,
    id: u64,
    bytes: Vec<u8>,
}

impl Working {
    fn new(store: Arc<Mutex<Store>>, id: u64) -> Self {
        Self {
            store,
            id,
            bytes: Vec::new(),
        }
    }

    fn extend(&mut self, bytes: &[u8]) -> bool {
        if self.bytes.len().saturating_add(bytes.len()) > PAYLOAD_LIMIT
            || !self
                .store
                .lock()
                .unwrap()
                .reserve_message_payload(self.id, bytes.len())
        {
            return false;
        }
        self.bytes.reserve_exact(bytes.len());
        self.bytes.extend_from_slice(bytes);
        true
    }

    fn publish(mut self, mut message: Message) {
        message.payload = mem::take(&mut self.bytes);
        self.store.lock().unwrap().message(self.id, message);
    }
}

impl Drop for Working {
    fn drop(&mut self) {
        if !self.bytes.is_empty() {
            self.store
                .lock()
                .unwrap()
                .release_message_payload(self.id, self.bytes.len());
        }
    }
}

pub(crate) struct Inspector {
    store: Arc<Mutex<Store>>,
    id: u64,
    direction: Direction,
    header: [u8; 14],
    header_read: usize,
    frame: Option<Frame>,
    data: Option<Data>,
    window: Option<Window>,
    decoder: Option<Decompress>,
}

impl Inspector {
    pub(crate) fn new(
        store: Arc<Mutex<Store>>,
        id: u64,
        direction: Direction,
        compression: Compression,
    ) -> Self {
        let window = match direction {
            Direction::ClientToServer => compression.client,
            Direction::ServerToClient => compression.server,
        };
        Self {
            store,
            id,
            direction,
            header: [0; 14],
            header_read: 0,
            frame: None,
            data: None,
            window,
            decoder: window.map(|_| Decompress::new(false)),
        }
    }

    pub(crate) fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.frame.is_none() {
                let take = if self.header_read < 2 {
                    (2 - self.header_read).min(bytes.len())
                } else {
                    let required =
                        2 + extended_length(self.header[1]) + mask_length(self.header[1]);
                    (required - self.header_read).min(bytes.len())
                };
                let end = self.header_read + take;
                self.header[self.header_read..end].copy_from_slice(&bytes[..take]);
                self.header_read = end;
                bytes = &bytes[take..];
                if end < 2
                    || end < 2 + extended_length(self.header[1]) + mask_length(self.header[1])
                {
                    continue;
                }
                self.start();
                self.header_read = 0;
            }
            let frame = self.frame.as_mut().unwrap();
            let len = (frame.size - frame.read).min(bytes.len() as u64) as usize;
            let mut payload = &bytes[..len];
            // Relay supplies at most one 8 KiB chunk, but accept arbitrary feed sizes.
            let mut scratch = [0; 8192];
            if let Some(mask) = frame.mask {
                let len = len.min(scratch.len());
                for (index, byte) in bytes[..len].iter().enumerate() {
                    scratch[index] = byte ^ mask[(frame.read as usize + index) % 4];
                }
                payload = &scratch[..len];
            }
            if frame.opcode == Opcode::Close(None) && frame.read < 2 {
                let offset = frame.read as usize;
                let keep = (2 - offset).min(payload.len());
                frame.close[offset..offset + keep].copy_from_slice(&payload[..keep]);
            }
            if matches!(
                frame.opcode,
                Opcode::Text | Opcode::Binary | Opcode::Continuation
            ) {
                if let Some(data) = self.data.as_mut().filter(|data| !data.skipped) {
                    if !data.bytes.extend(payload) {
                        data.skipped = true;
                        data.bytes = Working::new(self.store.clone(), self.id);
                    }
                }
            }
            frame.read += payload.len() as u64;
            bytes = &bytes[payload.len()..];
            if frame.read == frame.size {
                self.finish();
            }
        }
    }

    fn start(&mut self) {
        let [first, second, ..] = self.header;
        let extra = extended_length(second);
        let size = match extra {
            2 => u16::from_be_bytes(self.header[2..4].try_into().unwrap()) as u64,
            8 => u64::from_be_bytes(self.header[2..10].try_into().unwrap()),
            _ => (second & 0x7f) as u64,
        };
        let opcode = match first & 0xf {
            0 => Opcode::Continuation,
            1 => Opcode::Text,
            2 => Opcode::Binary,
            8 => Opcode::Close(None),
            9 => Opcode::Ping,
            10 => Opcode::Pong,
            other => Opcode::Other(other),
        };
        let compressed = first & 0x40 != 0;
        if matches!(opcode, Opcode::Text | Opcode::Binary) {
            // A new data frame replaces an invalid unfinished message.
            if self.data.take().is_some_and(|data| data.compressed) {
                self.lose_context();
            }
            self.data = Some(Data {
                text: opcode == Opcode::Text,
                compressed,
                bytes: Working::new(self.store.clone(), self.id),
                skipped: size > PAYLOAD_LIMIT as u64,
            });
        }
        self.frame = Some(Frame {
            opcode,
            finished: first & 0x80 != 0,
            compressed,
            mask: (second & 0x80 != 0)
                .then(|| self.header[2 + extra..6 + extra].try_into().unwrap()),
            size,
            read: 0,
            time: SystemTime::now(),
            close: [0; 2],
        });
    }

    fn finish(&mut self) {
        let frame = self.frame.take().unwrap();
        let mut message = Message {
            direction: self.direction,
            time: frame.time,
            opcode: match frame.opcode {
                Opcode::Close(_) if frame.size >= 2 => {
                    Opcode::Close(Some(u16::from_be_bytes(frame.close)))
                }
                opcode => opcode,
            },
            size: frame.size,
            payload: Vec::new(),
            text: false,
            note: None,
        };
        if matches!(
            frame.opcode,
            Opcode::Text | Opcode::Binary | Opcode::Continuation
        ) {
            if let Some(data) = self.data.as_mut() {
                if frame.opcode == Opcode::Continuation && frame.compressed {
                    data.skipped = true;
                }
            }
            if frame.finished {
                if let Some(data) = self.data.take() {
                    message.text = data.text;
                    if data.skipped {
                        message.note = Some("Payload exceeded capture limits");
                        if data.compressed {
                            self.lose_context();
                        }
                    } else if data.compressed {
                        if let Some(bytes) = self.inflate(&data.bytes.bytes) {
                            bytes.publish(message);
                            return;
                        }
                        self.lose_context();
                        message.note = Some("Could not decode permessage-deflate payload");
                    } else {
                        data.bytes.publish(message);
                        return;
                    }
                } else {
                    message.note = Some("Continuation without a data frame");
                }
            } else {
                message.note = Some("Payload continues in a later frame");
            }
        }
        self.store.lock().unwrap().message(self.id, message);
    }

    fn lose_context(&mut self) {
        self.decoder = self
            .window
            .filter(|window| window.reset)
            .map(|_| Decompress::new(false));
    }

    fn inflate(&mut self, compressed: &[u8]) -> Option<Working> {
        let decoder = self.decoder.as_mut()?;
        let mut output = Working::new(self.store.clone(), self.id);
        let mut buffer = [0; 8192];
        for mut input in [compressed, &[0, 0, 0xff, 0xff]] {
            loop {
                let (before_in, before_out) = (decoder.total_in(), decoder.total_out());
                decoder
                    .decompress(input, &mut buffer, FlushDecompress::Sync)
                    .ok()?;
                let consumed = (decoder.total_in() - before_in) as usize;
                let produced = (decoder.total_out() - before_out) as usize;
                if !output.extend(&buffer[..produced]) {
                    return None;
                }
                input = &input[consumed..];
                if input.is_empty() && produced < buffer.len() {
                    break;
                }
                if consumed == 0 && produced == 0 {
                    return None;
                }
            }
        }
        if self.window.is_some_and(|window| window.reset) {
            decoder.reset(false);
        }
        Some(output)
    }
}

fn extended_length(second: u8) -> usize {
    match second & 0x7f {
        126 => 2,
        127 => 8,
        _ => 0,
    }
}

fn mask_length(second: u8) -> usize {
    if second & 0x80 != 0 {
        4
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attribution, FlowKind, RequestHead};
    use flate2::{Compress, Compression as Level, FlushCompress};

    fn inspector(compression: Compression) -> Inspector {
        let mut store = Store::default();
        let id = store
            .start(
                0,
                Attribution::Unknown("test"),
                FlowKind::Upgrade,
                RequestHead {
                    method: "GET".into(),
                    url: "ws://example.com/".into(),
                    host: "example.com:80".into(),
                    path: "/".into(),
                    version: "HTTP/1.1".into(),
                    headers: Vec::new(),
                },
                &HeaderMap::new(),
            )
            .unwrap();
        Inspector::new(
            Arc::new(Mutex::new(store)),
            id,
            Direction::ClientToServer,
            compression,
        )
    }

    fn frame(first: u8, payload: &[u8], masked: bool) -> Vec<u8> {
        let mut result = vec![first, if masked { 0x80 } else { 0 }];
        match payload.len() {
            len @ 0..=125 => result[1] |= len as u8,
            len @ 126..=65535 => {
                result[1] |= 126;
                result.extend((len as u16).to_be_bytes());
            }
            len => {
                result[1] |= 127;
                result.extend((len as u64).to_be_bytes());
            }
        }
        if masked {
            let mask = [1, 2, 3, 4];
            result.extend(mask);
            result.extend(
                payload
                    .iter()
                    .enumerate()
                    .map(|(i, byte)| byte ^ mask[i % 4]),
            );
        } else {
            result.extend_from_slice(payload);
        }
        result
    }

    #[test]
    fn reads_split_headers_masking_fragments_and_control_frames() {
        let mut parser = inspector(Compression::default());
        let fragments = [
            frame(0x01, &[b'a', 0xf0, 0x9f], true),
            frame(0x89, b"ping", true),
            frame(0x80, &[0x98, 0x80], true),
            frame(0x8a, b"ping", true),
            frame(0x88, &[3, 232, b'!'], true),
            frame(0x82, &[0x7a, 0x95], true),
        ]
        .concat();
        for chunk in fragments.chunks(3) {
            parser.feed(chunk);
        }
        let flow = parser.store.lock().unwrap().flow(parser.id, false).unwrap();
        assert_eq!(flow.message_count, 6);
        assert_eq!(
            flow.messages.iter().map(|m| m.opcode).collect::<Vec<_>>(),
            [
                Opcode::Text,
                Opcode::Ping,
                Opcode::Continuation,
                Opcode::Pong,
                Opcode::Close(Some(1000)),
                Opcode::Binary,
            ]
        );
        assert_eq!(flow.messages[2].payload, "a😀".as_bytes());
        assert_eq!(flow.messages[2].size, 2);
        assert!(flow.messages[2].text);
        assert_eq!(flow.messages[5].payload, [0x7a, 0x95]);
    }

    fn compress(deflater: &mut Compress, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len() + 128);
        deflater
            .compress_vec(bytes, &mut output, FlushCompress::Sync)
            .unwrap();
        assert!(output.ends_with(&[0, 0, 0xff, 0xff]));
        output.truncate(output.len() - 4);
        output
    }

    #[test]
    fn negotiated_deflate_handles_fragments_and_context_takeover() {
        let mut request = HeaderMap::new();
        request.insert(
            "sec-websocket-extensions",
            "permessage-deflate; client_max_window_bits"
                .parse()
                .unwrap(),
        );
        let mut response = HeaderMap::new();
        response.insert(
            "sec-websocket-extensions",
            "permessage-deflate; server_no_context_takeover"
                .parse()
                .unwrap(),
        );
        let compression = Compression::negotiated(&request, &response);
        let mut parser = inspector(compression);
        let mut deflater = Compress::new(Level::best(), false);
        let text = b"the first message provides a dictionary for the second".repeat(100);
        let first = compress(&mut deflater, &text);
        let second = compress(&mut deflater, &text);
        assert!(second.len() < first.len());
        parser.feed(&frame(0x41, &first[..4], true));
        parser.feed(&frame(0x80, &first[4..], true));
        parser.feed(&frame(0xc1, &second, true));
        parser.feed(&frame(0xc1, &[0xff; 12], true));
        let flow = parser.store.lock().unwrap().flow(parser.id, false).unwrap();
        assert_eq!(flow.messages[1].payload, text);
        assert_eq!(flow.messages[2].payload, text);
        assert_eq!(flow.messages[3].size, 12);
        assert!(flow.messages[3].payload.is_empty());
        assert!(flow.messages[3].note.unwrap().contains("deflate"));
    }

    #[test]
    fn skips_oversized_frames_and_counts_over_the_frame_cap() {
        let mut parser = inspector(Compression::default());
        parser.feed(&frame(0x82, &vec![b'a'; PAYLOAD_LIMIT + 1], false));
        parser.feed(&frame(0x81, b"next frame", false));
        for _ in 0..FRAME_LIMIT {
            parser.feed(&frame(0x89, b"", false));
        }
        let mut store = parser.store.lock().unwrap();
        let flow = store.flow(parser.id, false).unwrap();
        assert_eq!(flow.messages[0].size, (PAYLOAD_LIMIT + 1) as u64);
        assert!(flow.messages[0].note.is_some());
        assert_eq!(flow.messages[1].payload, b"next frame");
        assert_eq!(flow.message_count, (FRAME_LIMIT + 2) as u64);
        assert_eq!(flow.messages.len(), FRAME_LIMIT);
        assert!(store.counters.memory > FRAME_LIMIT * std::mem::size_of::<Message>());
        store.clear();
        assert!(store.flow(parser.id, false).unwrap().messages.is_empty());
        assert!(store.counters.memory < 1024);
    }
}
