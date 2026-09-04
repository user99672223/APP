//! Length-prefixed framing for the control, `ctrl` and `chat` streams:
//! a `u32` little-endian byte length, then the postcard body.

use crate::ProtoError;
use serde::{de::DeserializeOwned, Serialize};

pub const LEN_PREFIX_BYTES: usize = 4;

pub fn encode_frame<T: Serialize + ?Sized>(
    value: &T,
    max_len: usize,
) -> Result<Vec<u8>, ProtoError> {
    let body = crate::encode(value)?;
    if body.len() > max_len {
        return Err(ProtoError::FrameTooLarge(body.len(), max_len));
    }
    let mut out = Vec::with_capacity(LEN_PREFIX_BYTES + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Incremental decoder for a byte stream of frames.
#[derive(Debug)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    max_len: usize,
}

impl FrameDecoder {
    pub fn new(max_len: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_len,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Next complete frame body, if one is buffered.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, ProtoError> {
        if self.buf.len() < LEN_PREFIX_BYTES {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > self.max_len {
            return Err(ProtoError::FrameTooLarge(len, self.max_len));
        }
        if self.buf.len() < LEN_PREFIX_BYTES + len {
            return Ok(None);
        }
        let body = self.buf[LEN_PREFIX_BYTES..LEN_PREFIX_BYTES + len].to_vec();
        self.buf.drain(..LEN_PREFIX_BYTES + len);
        Ok(Some(body))
    }

    pub fn next_message<T: DeserializeOwned>(&mut self) -> Result<Option<T>, ProtoError> {
        match self.next_frame()? {
            Some(body) => crate::decode(&body).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(feature = "tokio")]
pub mod aio {
    //! The same framing over tokio streams.

    use super::{encode_frame, LEN_PREFIX_BYTES};
    use crate::ProtoError;
    use serde::{de::DeserializeOwned, Serialize};
    use std::io;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    fn invalid(e: ProtoError) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }

    /// Read one frame body. `Ok(None)` on a clean end of stream at a frame boundary;
    /// an error if the stream ends inside a frame.
    pub async fn read_frame<R: AsyncRead + Unpin>(
        reader: &mut R,
        max_len: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut prefix = [0u8; LEN_PREFIX_BYTES];
        let mut filled = 0;
        while filled < LEN_PREFIX_BYTES {
            let n = reader.read(&mut prefix[filled..]).await?;
            if n == 0 {
                if filled == 0 {
                    return Ok(None);
                }
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            filled += n;
        }
        let len = u32::from_le_bytes(prefix) as usize;
        if len > max_len {
            return Err(invalid(ProtoError::FrameTooLarge(len, max_len)));
        }
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).await?;
        Ok(Some(body))
    }

    pub async fn read_message<R: AsyncRead + Unpin, T: DeserializeOwned>(
        reader: &mut R,
        max_len: usize,
    ) -> io::Result<Option<T>> {
        match read_frame(reader, max_len).await? {
            Some(body) => crate::decode(&body).map(Some).map_err(invalid),
            None => Ok(None),
        }
    }

    pub async fn write_message<W: AsyncWrite + Unpin, T: Serialize + ?Sized>(
        writer: &mut W,
        value: &T,
        max_len: usize,
    ) -> io::Result<()> {
        let frame = encode_frame(value, max_len).map_err(invalid)?;
        writer.write_all(&frame).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_across_pushes() {
        let a = encode_frame(&"hello".to_string(), 1024).unwrap();
        let b = encode_frame(&"world".to_string(), 1024).unwrap();
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut dec = FrameDecoder::new(1024);
        dec.push(&all[..3]);
        assert!(dec.next_message::<String>().unwrap().is_none());
        dec.push(&all[3..a.len() + 2]);
        assert_eq!(
            dec.next_message::<String>().unwrap().as_deref(),
            Some("hello")
        );
        assert!(dec.next_message::<String>().unwrap().is_none());
        dec.push(&all[a.len() + 2..]);
        assert_eq!(
            dec.next_message::<String>().unwrap().as_deref(),
            Some("world")
        );
        assert_eq!(dec.buffered(), 0);
    }

    #[test]
    fn oversized_frames_are_rejected() {
        assert!(matches!(
            encode_frame(&vec![0u8; 100], 50),
            Err(ProtoError::FrameTooLarge(_, 50))
        ));
        let mut dec = FrameDecoder::new(8);
        dec.push(&1000u32.to_le_bytes());
        assert!(matches!(
            dec.next_frame(),
            Err(ProtoError::FrameTooLarge(1000, 8))
        ));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn async_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(64);
        aio::write_message(&mut a, &(1u8, "x".to_string()), 64)
            .await
            .unwrap();
        aio::write_message(&mut a, &(2u8, "y".to_string()), 64)
            .await
            .unwrap();
        drop(a);
        let first: Option<(u8, String)> = aio::read_message(&mut b, 64).await.unwrap();
        assert_eq!(first, Some((1, "x".to_string())));
        let second: Option<(u8, String)> = aio::read_message(&mut b, 64).await.unwrap();
        assert_eq!(second, Some((2, "y".to_string())));
        let end: Option<(u8, String)> = aio::read_message(&mut b, 64).await.unwrap();
        assert_eq!(end, None);
    }
}
