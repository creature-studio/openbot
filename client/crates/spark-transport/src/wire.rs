//! Async framed I/O for the SSH bridge.
//!
//! The **wire format** lives in `sand_protocol::frame` (one definition, shared
//! with `sandd` and `sand bridge`); this module only adds the async read/write
//! loop the client needs. The framing itself is:
//!
//! ```text
//! [u16 protocol_version][u16 kind][u64 request_id][u64 stream_id][u32 payload_len]
//! [payload: [u32 json_len][json][binary]]
//! ```

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use sand_protocol::frame::{FrameHeader, FramePayload, HEADER_LEN, MAX_PAYLOAD_LEN};

/// Read one full frame (header + payload) from `reader`.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<(FrameHeader, Vec<u8>)> {
    let mut header_buf = [0u8; HEADER_LEN];
    reader.read_exact(&mut header_buf).await?;
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
        reader.read_exact(&mut payload).await?;
    }
    Ok((header, payload))
}

/// Write one full frame (header + payload) and flush it.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    header: &FrameHeader,
    payload: &[u8],
) -> io::Result<()> {
    if payload.len() as u64 > MAX_PAYLOAD_LEN as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("payload too large: {}", payload.len()),
        ));
    }
    let mut header = *header;
    header.payload_len = payload.len() as u32;
    writer.write_all(&header.encode()).await?;
    if !payload.is_empty() {
        writer.write_all(payload).await?;
    }
    writer.flush().await
}

/// Encode a `json + binary` request body (same framing `sandd` uses).
pub fn encode_payload(payload: &FramePayload) -> Vec<u8> {
    payload.encode()
}

/// Decode a `json + binary` body.
pub fn decode_payload(bytes: &[u8]) -> io::Result<FramePayload> {
    FramePayload::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sand_protocol::frame::{FrameKind, BRIDGE_PROTOCOL_VERSION};

    #[tokio::test]
    async fn frames_round_trip_through_duplex() {
        let (mut client, mut server) = tokio::io::duplex(4096);

        let payload = FramePayload::with_binary("{\"method\":\"Exec\"}", b"raw-bytes".to_vec());
        let encoded = encode_payload(&payload);
        let header = FrameHeader::new(FrameKind::Request, 7, 0, 0);
        write_frame(&mut client, &header, &encoded).await.unwrap();

        let (got_header, got_payload) = read_frame(&mut server).await.unwrap();
        assert_eq!(got_header.kind, FrameKind::Request);
        assert_eq!(got_header.request_id, 7);
        assert_eq!(got_header.protocol_version, BRIDGE_PROTOCOL_VERSION);
        let decoded = decode_payload(&got_payload).unwrap();
        assert_eq!(decoded.json, "{\"method\":\"Exec\"}");
        assert_eq!(decoded.binary, b"raw-bytes");
    }

    #[tokio::test]
    async fn rejects_oversized_payload_len() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let mut header = FrameHeader::new(FrameKind::Request, 1, 0, 0);
        header.payload_len = MAX_PAYLOAD_LEN + 1;
        client.write_all(&header.encode()).await.unwrap();
        let err = read_frame(&mut server).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn eof_surfaces_as_unexpected_eof() {
        let (client, mut server) = tokio::io::duplex(64);
        drop(client);
        let err = read_frame(&mut server).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
