//! Async frame codec over the term-contracts wire format (`01-contracts.md`
//! §3: `u32 LE JSON byte length + UTF-8 JSON`, whole frame ≤ 65,536 bytes).
//!
//! Decode reuses `term_contracts::rpc::decode_frame` on an in-memory cursor
//! so bridge and daemon stay byte-compatible on every validation rule
//! (size, UTF-8, JSON, nesting depth ≤ 32). Encode uses the contracts
//! encoder directly.

use serde_json::Value;
use term_contracts::rpc::{self, FrameError, MAX_FRAME_BYTES};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Read exactly one frame. The length header is validated before the body is
/// allocated so a poisoned length can never trigger a huge allocation.
pub async fn read_frame<R>(reader: &mut R) -> Result<Value, FrameError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let mut header = [0u8; 4];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|_| FrameError::Truncated)?;
    let len = u32::from_le_bytes(header);
    if len as usize + 4 > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len, MAX_FRAME_BYTES as u32));
    }
    let mut framed = vec![0u8; 4 + len as usize];
    framed[..4].copy_from_slice(&header);
    reader
        .read_exact(&mut framed[4..])
        .await
        .map_err(|_| FrameError::Truncated)?;
    let mut cursor = std::io::Cursor::new(framed);
    rpc::decode_frame(&mut cursor)
}

/// Encode and write one frame; refuses to write a frame whose encoded size
/// (JSON escaping + metadata included) exceeds the 64 KiB budget.
pub async fn write_frame<W>(writer: &mut W, value: &Value) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    let bytes = rpc::encode_frame(value)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use term_contracts::rpc::{Frame, RpcEvent, RpcEventKind, RpcRequest};
    use tokio::io::duplex;

    #[tokio::test]
    async fn frames_round_trip_over_a_duplex_stream() {
        let (mut client, mut daemon) = duplex(8 * 1024);
        let request = RpcRequest::new("req-1", "system.snapshot", json!({}));
        let event = RpcEvent {
            v: 1,
            event: RpcEventKind::QueueChanged,
            payload: json!({"revision": 3}),
        };
        let values = [
            serde_json::to_value(&request).unwrap(),
            serde_json::to_value(&event).unwrap(),
            json!({"v": 1, "id": "req-1", "result": {"ok": true}}),
        ];

        let writer = tokio::spawn(async move {
            for value in &values {
                write_frame(&mut daemon, value).await.unwrap();
            }
        });

        let decoded_request = Frame::from_json(read_frame(&mut client).await.unwrap()).unwrap();
        let decoded_event = Frame::from_json(read_frame(&mut client).await.unwrap()).unwrap();
        let decoded_response = Frame::from_json(read_frame(&mut client).await.unwrap()).unwrap();
        writer.await.unwrap();

        assert_eq!(decoded_request, Frame::Request(request));
        assert_eq!(
            decoded_event,
            Frame::Event(RpcEvent {
                v: 1,
                event: RpcEventKind::QueueChanged,
                payload: json!({"revision": 3})
            })
        );
        assert!(matches!(decoded_response, Frame::Response(_)));
    }

    #[tokio::test]
    async fn oversized_length_header_is_rejected_before_allocation() {
        let (mut client, mut daemon) = duplex(128);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            daemon.write_all(&(70_000u32).to_le_bytes()).await.unwrap();
        });
        let err = read_frame(&mut client).await.unwrap_err();
        writer.await.unwrap();
        assert!(matches!(err, FrameError::TooLarge(70_000, _)));
    }

    #[tokio::test]
    async fn truncated_body_is_an_error() {
        let (mut client, mut daemon) = duplex(128);
        let value = json!({"hello": "world"});
        let mut bytes = term_contracts::rpc::encode_frame(&value).unwrap();
        bytes.truncate(bytes.len() - 3);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            daemon.write_all(&bytes).await.unwrap();
        });
        let err = read_frame(&mut client).await.unwrap_err();
        writer.await.unwrap();
        assert!(matches!(err, FrameError::Truncated));
    }

    #[tokio::test]
    async fn invalid_utf8_body_is_rejected() {
        let (mut client, mut daemon) = duplex(128);
        let mut bytes = 4u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0x00, 0x01]);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            daemon.write_all(&bytes).await.unwrap();
        });
        let err = read_frame(&mut client).await.unwrap_err();
        writer.await.unwrap();
        assert!(matches!(err, FrameError::NotUtf8));
    }

    #[tokio::test]
    async fn oversize_value_is_refused_on_write() {
        let (mut client, mut daemon) = duplex(1024);
        let big = json!({"data": "x".repeat(70_000)});
        let err = write_frame(&mut daemon, &big).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge(_, _)));
        drop(daemon);
        // Reader sees clean EOF → truncated.
        assert!(matches!(
            read_frame(&mut client).await,
            Err(FrameError::Truncated)
        ));
    }
}
