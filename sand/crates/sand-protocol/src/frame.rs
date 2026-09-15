//! Binary framed protocol for the SSH stdio bridge.
//!
//! This is the wire format that travels over `ssh` stdin/stdout between the
//! host-agent (client side) and `sand bridge` (remote side):
//!
//! ```text
//! host-agent ── framed binary ──▶ ssh(1) ──▶ sand bridge ──▶ remote sandd UDS
//! ```
//!
//! Frame layout (all integers big-endian):
//!
//! ```text
//! ┌──────────────┬──────────────┬──────────────┬──────────────┬──────────────┐
//! │   version    │    kind      │  request_id  │  stream_id   │  payload_len │
//! │    (u16)     │    (u16)     │    (u64)     │    (u64)     │    (u32)     │
//! ├──────────────┴──────────────┴──────────────┴──────────────┴──────────────┤
//! │                             payload (payload_len bytes)                  │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Header = 2 + 2 + 8 + 8 + 4 = 24 bytes.
//!
//! Every payload (Request / Response / StreamOpen / StreamData / StreamClose /
//! Event) uses the same encoding, shared with sandd's binary RPC framing:
//!
//! ```text
//! ┌───────────────┬──────────────────┬──────────────────────┐
//! │  json_len     │  json (utf8)     │  binary (optional)   │
//! │  (u32, BE)    │  json_len bytes  │  rest of payload     │
//! └───────────────┴──────────────────┴──────────────────────┘
//! ```
//!
//! Keeping one encoding for every payload means the bridge is a thin
//! forwarder: a `Request` payload can be copied byte-for-byte into the remote
//! sandd binary socket, and a sandd response can be copied back into a
//! `Response` payload. No base64, no double parsing.
//!
//! `Ping` / `Pong` frames carry an empty payload and are answered by the
//! bridge itself, so latency measurement covers the whole
//! `host-agent → ssh → bridge` path without touching sandd.

use std::io::{self, Read, Write};

/// Version of the bridge framing protocol (independent of the sandd binary
/// version and of the sand RPC protocol version).
pub const BRIDGE_PROTOCOL_VERSION: u16 = 1;

/// Hard cap on a single frame payload. Protects both sides from a corrupt or
/// hostile `payload_len`.
pub const MAX_PAYLOAD_LEN: u32 = 8 * 1024 * 1024;

/// Size of the encoded frame header.
pub const HEADER_LEN: usize = 24;

// ---------------------------------------------------------------------------
// FrameKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// host-agent → bridge: RPC request (payload = json + optional binary).
    Request = 1,
    /// bridge → host-agent: RPC response (payload = json + optional binary).
    Response = 2,
    /// host-agent → bridge: open a long-lived stream.
    StreamOpen = 3,
    /// bridge → host-agent: data on an open stream.
    StreamData = 4,
    /// either direction: close a stream.
    StreamClose = 5,
    /// bridge → host-agent: asynchronous runtime event.
    Event = 6,
    /// either direction: keepalive / latency probe.
    Ping = 7,
    /// answer to `Ping`.
    Pong = 8,
}

impl FrameKind {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(FrameKind::Request),
            2 => Some(FrameKind::Response),
            3 => Some(FrameKind::StreamOpen),
            4 => Some(FrameKind::StreamData),
            5 => Some(FrameKind::StreamClose),
            6 => Some(FrameKind::Event),
            7 => Some(FrameKind::Ping),
            8 => Some(FrameKind::Pong),
            _ => None,
        }
    }

    pub fn as_u16(self) -> u16 {
        self as u16
    }

    pub fn name(self) -> &'static str {
        match self {
            FrameKind::Request => "Request",
            FrameKind::Response => "Response",
            FrameKind::StreamOpen => "StreamOpen",
            FrameKind::StreamData => "StreamData",
            FrameKind::StreamClose => "StreamClose",
            FrameKind::Event => "Event",
            FrameKind::Ping => "Ping",
            FrameKind::Pong => "Pong",
        }
    }
}

// ---------------------------------------------------------------------------
// FrameHeader
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub protocol_version: u16,
    pub kind: FrameKind,
    pub request_id: u64,
    pub stream_id: u64,
    pub payload_len: u32,
}

impl FrameHeader {
    pub fn new(kind: FrameKind, request_id: u64, stream_id: u64, payload_len: u32) -> Self {
        Self {
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            kind,
            request_id,
            stream_id,
            payload_len,
        }
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut buf = [0u8; HEADER_LEN];
        buf[0..2].copy_from_slice(&self.protocol_version.to_be_bytes());
        buf[2..4].copy_from_slice(&self.kind.as_u16().to_be_bytes());
        buf[4..12].copy_from_slice(&self.request_id.to_be_bytes());
        buf[12..20].copy_from_slice(&self.stream_id.to_be_bytes());
        buf[20..24].copy_from_slice(&self.payload_len.to_be_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Option<FrameHeader> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let protocol_version = u16::from_be_bytes([buf[0], buf[1]]);
        let kind = FrameKind::from_u16(u16::from_be_bytes([buf[2], buf[3]]))?;
        let request_id = u64::from_be_bytes([
            buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
        ]);
        let stream_id = u64::from_be_bytes([
            buf[12], buf[13], buf[14], buf[15], buf[16], buf[17], buf[18], buf[19],
        ]);
        let payload_len = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
        Some(FrameHeader {
            protocol_version,
            kind,
            request_id,
            stream_id,
            payload_len,
        })
    }
}

// ---------------------------------------------------------------------------
// Frame IO
// ---------------------------------------------------------------------------

/// Read one complete frame. Blocks until the header and payload arrive.
/// Returns error kind `UnexpectedEof` when the peer closed the connection.
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<(FrameHeader, Vec<u8>)> {
    let mut header_buf = [0u8; HEADER_LEN];
    reader.read_exact(&mut header_buf)?;
    let header = FrameHeader::decode(&header_buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid frame header"))?;

    if header.payload_len > MAX_PAYLOAD_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame payload too large: {}", header.payload_len),
        ));
    }

    let mut payload = vec![0u8; header.payload_len as usize];
    if header.payload_len > 0 {
        reader.read_exact(&mut payload)?;
    }
    Ok((header, payload))
}

/// Write one complete frame and flush it.
pub fn write_frame<W: Write>(writer: &mut W, header: &FrameHeader, payload: &[u8]) -> io::Result<()> {
    if payload.len() as u64 > MAX_PAYLOAD_LEN as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("payload too large: {}", payload.len()),
        ));
    }
    let mut header = *header;
    header.payload_len = payload.len() as u32;
    writer.write_all(&header.encode())?;
    if !payload.is_empty() {
        writer.write_all(payload)?;
    }
    writer.flush()
}

// ---------------------------------------------------------------------------
// FramePayload — json + optional binary, shared with sandd binary RPC
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FramePayload {
    pub json: String,
    pub binary: Vec<u8>,
}

impl FramePayload {
    pub fn json(json: impl Into<String>) -> Self {
        Self {
            json: json.into(),
            binary: Vec::new(),
        }
    }

    pub fn with_binary(json: impl Into<String>, binary: Vec<u8>) -> Self {
        Self {
            json: json.into(),
            binary,
        }
    }

    /// `[u32 json_len][json][binary]` — identical to sandd's binary framing.
    pub fn encode(&self) -> Vec<u8> {
        let json_bytes = self.json.as_bytes();
        let mut out = Vec::with_capacity(4 + json_bytes.len() + self.binary.len());
        out.extend_from_slice(&(json_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(json_bytes);
        out.extend_from_slice(&self.binary);
        out
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload shorter than json_len prefix",
            ));
        }
        let json_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        if bytes.len() < 4 + json_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload shorter than declared json_len",
            ));
        }
        let json = String::from_utf8_lossy(&bytes[4..4 + json_len]).to_string();
        let binary = bytes[4 + json_len..].to_vec();
        Ok(FramePayload { json, binary })
    }

    pub fn is_empty(&self) -> bool {
        self.json.is_empty() && self.binary.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Minimal JSON helpers (sandd's JSON layer is hand rolled; keep it that way)
// ---------------------------------------------------------------------------

pub mod json {
    /// Extract a string field: `"field":"value"` (also tolerates spaces).
    pub fn get_str(json: &str, field: &str) -> Option<String> {
        let patterns = [
            format!("\"{}\":\"", field),
            format!("\"{}\": \"", field),
            format!("\"{}\" : \"", field),
        ];
        for pat in &patterns {
            if let Some(start) = json.find(pat.as_str()) {
                let rest = &json[start + pat.len()..];
                if let Some(end) = rest.find('"') {
                    return Some(unescape(&rest[..end]));
                }
            }
        }
        None
    }

    /// Extract an integer field: `"field":123`.
    pub fn get_u64(json: &str, field: &str) -> Option<u64> {
        let pat = format!("\"{}\":", field);
        let start = json.find(pat.as_str())?;
        let rest = json[start + pat.len()..].trim_start();
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        rest[..end].parse().ok()
    }

    /// Extract a bool field: `"field":true`.
    pub fn get_bool(json: &str, field: &str) -> Option<bool> {
        let pat = format!("\"{}\":", field);
        let start = json.find(pat.as_str())?;
        let rest = json[start + pat.len()..].trim_start();
        if rest.starts_with("true") {
            Some(true)
        } else if rest.starts_with("false") {
            Some(false)
        } else {
            None
        }
    }

    /// True when the response is marked successful (`"ok":true`).
    pub fn is_ok(json: &str) -> bool {
        get_bool(json, "ok").unwrap_or(false)
    }

    /// Pull `"error":"..."` out of a failed response.
    pub fn error_of(json: &str) -> Option<String> {
        get_str(json, "error")
    }

    pub fn escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    fn unescape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                }
                Some(other) => out.push(other),
                None => break,
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn header_roundtrip() {
        let header = FrameHeader::new(FrameKind::Request, 42, 7, 1234);
        let encoded = header.encode();
        assert_eq!(encoded.len(), HEADER_LEN);
        let decoded = FrameHeader::decode(&encoded).expect("decode");
        assert_eq!(decoded, header);
    }

    #[test]
    fn frame_roundtrip_with_binary() {
        let payload = FramePayload::with_binary("{\"method\":\"Exec\"}", vec![0, 1, 2, 255]);
        let bytes = payload.encode();
        let mut cursor = Cursor::new(bytes);
        let (header, raw) = read_frame(&mut cursor).expect("read");
        assert_eq!(header.kind, FrameKind::Request);
        assert_eq!(header.payload_len as usize, raw.len());
        let decoded = FramePayload::decode(&raw).expect("decode payload");
        assert_eq!(decoded.json, "{\"method\":\"Exec\"}");
        assert_eq!(decoded.binary, vec![0, 1, 2, 255]);
    }

    #[test]
    fn write_frame_fills_payload_len() {
        let mut out: Vec<u8> = Vec::new();
        // payload_len on the header is deliberately wrong; write_frame fixes it
        let header = FrameHeader::new(FrameKind::Pong, 0, 0, 999);
        write_frame(&mut out, &header, b"abc").expect("write");
        assert_eq!(&out[20..24], &3u32.to_be_bytes());
        assert_eq!(&out[24..], b"abc");
    }

    #[test]
    fn rejects_oversized_payload() {
        let mut header = FrameHeader::new(FrameKind::Response, 0, 0, 0).encode().to_vec();
        header[20..24].copy_from_slice(&(MAX_PAYLOAD_LEN + 1).to_be_bytes());
        let mut cursor = Cursor::new(header);
        let err = read_frame(&mut cursor).expect_err("must reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn json_helpers() {
        let s = "{\"ok\":true,\"id\":\"rt-1\",\"cols\":120,\"error\":\"nope\\\"x\"}";
        assert_eq!(json::get_str(s, "id").as_deref(), Some("rt-1"));
        assert_eq!(json::get_u64(s, "cols"), Some(120));
        assert!(json::get_bool(s, "ok").unwrap());
        assert!(json::is_ok(s));
        assert!(json::escape("a\"b\n").starts_with("a\\\"b"));
    }
}
