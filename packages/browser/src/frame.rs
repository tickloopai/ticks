//! The relay's framing: a 4-byte length, then that many bytes of JSON, as Chrome's native messaging
//! frames its messages. The socket between the native host and the Tick uses the same framing, so the
//! host relays frames without reading them.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Chrome's limit for a message from a native host, and so for anything the Tick sends the extension.
pub const TO_EXTENSION_MAX_BYTES: usize = 1024 * 1024;
/// Chrome's limit for a message to a native host, and so for anything the extension sends the Tick.
pub const FROM_EXTENSION_MAX_BYTES: usize = 64 * 1024 * 1024;

/// The most memory set aside for a message before its bytes arrive.
const READ_CHUNK_BYTES: usize = 64 * 1024;

// Chrome writes the length in native byte order, and the socket hop names little-endian; every
// target this package ships for is little-endian, so the two agree.
#[cfg(not(target_endian = "little"))]
compile_error!("the relay framing assumes a little-endian target");

/// `message` with its length in front, refused when it is longer than `maximum`.
pub fn encode(message: &[u8], maximum: usize) -> io::Result<Vec<u8>> {
    if message.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a relay message of {} bytes exceeds {maximum}",
                message.len()
            ),
        ));
    }
    let length = u32::try_from(message.len()).expect("the limits fit a 32-bit length");
    let mut frame = Vec::with_capacity(4 + message.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(message);
    Ok(frame)
}

/// Reads the next message: `None` when the peer closed between messages. A length over `maximum`
/// or a stream that ends inside a message is an error, after which the stream is unusable.
pub async fn read<R: AsyncRead + Unpin>(
    reader: &mut R,
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    let mut filled = 0;
    while filled < length.len() {
        let read = reader.read(&mut length[filled..]).await?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        filled += read;
    }
    let length = u32::from_le_bytes(length) as usize;
    if length > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a relay message of {length} bytes exceeds {maximum}"),
        ));
    }
    // The message grows as its bytes arrive, so a length alone holds no memory.
    let mut message = Vec::with_capacity(length.min(READ_CHUNK_BYTES));
    (&mut *reader)
        .take(length as u64)
        .read_to_end(&mut message)
        .await?;
    if message.len() < length {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some(message))
}

/// Writes `message` as one frame and flushes it.
pub async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &[u8],
    maximum: usize,
) -> io::Result<()> {
    writer.write_all(&encode(message, maximum)?).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_message_round_trips_behind_its_little_endian_length() {
        let frame = encode(br#"{"type":"stop"}"#, TO_EXTENSION_MAX_BYTES).unwrap();
        assert_eq!(&frame[..4], &[15, 0, 0, 0]);
        let mut stream = &frame[..];
        let message = read(&mut stream, TO_EXTENSION_MAX_BYTES).await.unwrap();
        assert_eq!(message.as_deref(), Some(&br#"{"type":"stop"}"#[..]));
        assert_eq!(
            read(&mut stream, TO_EXTENSION_MAX_BYTES).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn messages_follow_each_other_in_one_stream() {
        let mut bytes = encode(b"{}", 16).unwrap();
        bytes.extend(encode(b"[1]", 16).unwrap());
        let mut stream = &bytes[..];
        assert_eq!(read(&mut stream, 16).await.unwrap().unwrap(), b"{}");
        assert_eq!(read(&mut stream, 16).await.unwrap().unwrap(), b"[1]");
        assert!(read(&mut stream, 16).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_message_over_the_limit_is_neither_written_nor_read() {
        assert!(encode(&[b' '; 17], 16).is_err());
        assert_eq!(encode(&[b' '; 16], 16).unwrap().len(), 20);
        let frame = encode(&[b' '; 17], 32).unwrap();
        let error = read(&mut &frame[..], 16).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let over = (TO_EXTENSION_MAX_BYTES as u32 + 1).to_le_bytes();
        assert!(read(&mut &over[..], TO_EXTENSION_MAX_BYTES).await.is_err());
    }

    #[tokio::test]
    async fn a_length_alone_sets_no_memory_aside_and_waits_for_its_bytes() {
        // The length 0x03FFFFFF and nothing after it, on a connection that stays open.
        let (mut client, mut server) = tokio::io::duplex(64);
        client
            .write_all(&0x03FF_FFFFu32.to_le_bytes())
            .await
            .unwrap();
        let read = read(&mut server, FROM_EXTENSION_MAX_BYTES);
        tokio::pin!(read);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut read)
                .await
                .is_err()
        );
        drop(client);
        assert_eq!(read.await.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn a_stream_cut_inside_a_message_is_an_error() {
        let frame = encode(b"{\"type\":\"hello\"}", 64).unwrap();
        for cut in [2, 4, 9] {
            let error = read(&mut &frame[..cut], 64).await.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof, "cut at {cut}");
        }
    }
}
