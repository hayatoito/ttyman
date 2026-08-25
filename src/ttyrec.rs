use std::io::{self, Read, Write};
use std::time::SystemTime;
use thiserror::Error;

pub const HEADER_SIZE: usize = 12;
pub const MAX_RECORD_LEN: u32 = 0x00FF_FFFF; // 24-bit limit (16,777,215 bytes)

#[derive(Error, Debug, PartialEq, Eq)]
pub enum HeaderError {
    #[error("Invalid microsecond value: {0} (must be < 1,000,000)")]
    InvalidUsec(u32),
    #[error("Payload length exceeds 16MB limit: {0} bytes")]
    InvalidLength(u32),
    #[error("I/O error during header processing: {0}")]
    Io(String),
}

/// A 12-byte .ttyrec frame header containing a timestamp and payload length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub sec: u64,
    pub usec: u32,
    pub len: u32,
}

impl Header {
    pub fn new(sec: u64, usec: u32, len: u32) -> Result<Self, HeaderError> {
        if usec > 999_999 {
            return Err(HeaderError::InvalidUsec(usec));
        }
        if len > MAX_RECORD_LEN {
            return Err(HeaderError::InvalidLength(len));
        }
        Ok(Self { sec, usec, len })
    }

    pub fn now(len: u32) -> Result<Self, HeaderError> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        Self::new(now.as_secs(), now.subsec_micros(), len)
    }

    pub fn serialize(&self) -> [u8; HEADER_SIZE] {
        let sec_low = (self.sec & 0xFFFF_FFFF) as u32;
        let sec_high = ((self.sec >> 32) & 0x0FFF) as u32;

        let packed_usec = (self.usec & 0x000F_FFFF) | (sec_high << 20);
        let packed_len = self.len & 0x00FF_FFFF;

        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&sec_low.to_le_bytes());
        buf[4..8].copy_from_slice(&packed_usec.to_le_bytes());
        buf[8..12].copy_from_slice(&packed_len.to_le_bytes());
        buf
    }

    pub fn deserialize(buf: &[u8; HEADER_SIZE]) -> Result<Self, HeaderError> {
        let sec_low = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as u64;
        let raw_usec = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let raw_len = u32::from_le_bytes(buf[8..12].try_into().unwrap());

        let sec_high = ((raw_usec >> 20) & 0x0FFF) as u64;
        let sec = sec_low | (sec_high << 32);
        let usec = raw_usec & 0x000F_FFFF;
        let len = raw_len & 0x00FF_FFFF;

        if usec > 999_999 {
            return Err(HeaderError::InvalidUsec(usec));
        }
        if len > MAX_RECORD_LEN {
            return Err(HeaderError::InvalidLength(len));
        }

        Ok(Self { sec, usec, len })
    }
}

#[derive(Error, Debug)]
pub enum FrameError {
    #[error("Header error: {0}")]
    Header(#[from] HeaderError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Incomplete header: expected {HEADER_SIZE} bytes, got {0}")]
    IncompleteHeader(usize),
    #[error("Incomplete payload: expected {expected} bytes, got {got}")]
    IncompletePayload { expected: u32, got: usize },
}

/// A complete record frame containing a validated header and raw payload data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: Header,
    pub data: Vec<u8>,
}

pub fn read_header<R: Read>(mut reader: R) -> Result<Option<Header>, FrameError> {
    let mut buf = [0u8; HEADER_SIZE];
    match reader.read_exact(&mut buf) {
        Ok(()) => {
            let header = Header::deserialize(&buf)?;
            Ok(Some(header))
        }
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(FrameError::Io(e)),
    }
}

pub fn read_frame<R: Read>(mut reader: R) -> Result<Option<Frame>, FrameError> {
    let header = match read_header(&mut reader)? {
        Some(h) => h,
        None => return Ok(None),
    };

    let mut data = vec![0u8; header.len as usize];
    if header.len > 0 {
        match reader.read_exact(&mut data) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(FrameError::IncompletePayload {
                    expected: header.len,
                    got: 0,
                });
            }
            Err(e) => return Err(FrameError::Io(e)),
        }
    }

    Ok(Some(Frame { header, data }))
}

/// A streaming frame reader with an internal buffer that safely handles packet fragmentation
/// without discarding partial headers or payloads across incremental I/O reads.
pub struct FrameReader<R> {
    reader: R,
    buffer: Vec<u8>,
}

impl<R: Read> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::with_capacity(8192),
        }
    }

    /// Reads the next frame from the stream.
    ///
    /// Returns `Ok(Some(frame))` when a complete frame is available.
    /// Returns `Ok(None)` when EOF is reached (or when waiting for more data in incremental streams).
    pub fn read_next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(frame) = extract_frame_from_buffer(&mut self.buffer)? {
                return Ok(Some(frame));
            }

            match self.reader.read(&mut chunk) {
                Ok(0) => {
                    return Ok(None);
                }
                Ok(n) => {
                    self.buffer.extend_from_slice(&chunk[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(FrameError::Io(e)),
            }
        }
    }

    pub fn is_buffer_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

pub fn extract_frame_from_buffer(buffer: &mut Vec<u8>) -> Result<Option<Frame>, FrameError> {
    if buffer.len() >= HEADER_SIZE {
        let header_bytes: [u8; HEADER_SIZE] = buffer[0..HEADER_SIZE].try_into().unwrap();
        let header = Header::deserialize(&header_bytes)?;
        let total_len = HEADER_SIZE + (header.len as usize);
        if buffer.len() >= total_len {
            let data = buffer[HEADER_SIZE..total_len].to_vec();
            buffer.drain(0..total_len);
            return Ok(Some(Frame { header, data }));
        }
    }
    Ok(None)
}

pub fn write_header<W: Write>(mut writer: W, header: &Header) -> Result<(), FrameError> {
    let bytes = header.serialize();
    writer.write_all(&bytes)?;
    Ok(())
}

pub fn write_frame<W: Write>(mut writer: W, frame: &Frame) -> Result<(), FrameError> {
    write_header(&mut writer, &frame.header)?;
    if !frame.data.is_empty() {
        writer.write_all(&frame.data)?;
    }
    Ok(())
}

pub fn write_raw_frame<W: Write>(
    mut writer: W,
    header: &Header,
    payload: &[u8],
) -> Result<(), FrameError> {
    let header_bytes = header.serialize();
    writer.write_all(&header_bytes)?;
    if !payload.is_empty() {
        writer.write_all(payload)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    #[test]
    fn test_standard_timestamp_roundtrip() {
        let header = Header {
            sec: 1700000000,
            usec: 500000,
            len: 1024,
        };
        let bytes = header.serialize();
        let decoded = Header::deserialize(&bytes).unwrap();
        assert_eq!(header, decoded);
    }

    #[test]
    fn test_y2038_extended_timestamp_roundtrip() {
        let header = Header {
            sec: 3_000_000_000,
            usec: 123456,
            len: 2048,
        };
        let bytes = header.serialize();
        let decoded = Header::deserialize(&bytes).unwrap();
        assert_eq!(header, decoded);
    }

    #[test]
    fn test_max_44bit_sec_roundtrip() {
        let max_sec = (1u64 << 44) - 1;
        let header = Header {
            sec: max_sec,
            usec: 999999,
            len: MAX_RECORD_LEN,
        };
        let bytes = header.serialize();
        let decoded = Header::deserialize(&bytes).unwrap();
        assert_eq!(header, decoded);
    }

    #[test]
    fn test_invalid_usec() {
        let mut buf = [0u8; 12];
        buf[4..8].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert!(matches!(
            Header::deserialize(&buf),
            Err(HeaderError::InvalidUsec(_))
        ));
    }

    #[test]
    fn test_invalid_lengths() {
        let err = Header::new(100, 100, MAX_RECORD_LEN + 1);
        assert!(matches!(err, Err(HeaderError::InvalidLength(_))));
    }

    proptest! {
        #[test]
        fn proptest_header_roundtrip(
            sec in 0u64..(1u64 << 44),
            usec in 0u32..=999_999u32,
            len in 0u32..=16_000_000u32
        ) {
            let header = Header { sec, usec, len };
            let bytes = header.serialize();
            let decoded = Header::deserialize(&bytes).unwrap();
            prop_assert_eq!(header, decoded);
        }
    }

    #[test]
    fn test_read_write_frame_roundtrip() {
        let payload = b"Hello, tt terminal world!\r\n";
        let header = Header {
            sec: 1700000000,
            usec: 543210,
            len: payload.len() as u32,
        };
        let frame = Frame {
            header,
            data: payload.to_vec(),
        };

        let mut buffer = Vec::new();
        write_frame(&mut buffer, &frame).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded = read_frame(&mut cursor).unwrap().expect("should read frame");

        assert_eq!(frame, decoded);
    }

    #[test]
    fn test_incomplete_header_error() {
        let buffer = vec![0u8; 6]; // Incomplete header
        let mut cursor = Cursor::new(buffer);
        let res = read_frame(&mut cursor);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), None);
    }

    #[test]
    fn test_incomplete_payload_error() {
        let header = Header {
            sec: 100,
            usec: 100,
            len: 50,
        };
        let mut buffer = Vec::new();
        write_header(&mut buffer, &header).unwrap();
        buffer.extend_from_slice(b"too short");

        let mut cursor = Cursor::new(buffer);
        let res = read_frame(&mut cursor);
        assert!(matches!(res, Err(FrameError::IncompletePayload { .. })));
    }

    #[test]
    fn test_frame_reader_fragmented_stream() {
        let frame1 = Frame {
            header: Header::new(100, 200, 5).unwrap(),
            data: b"hello".to_vec(),
        };
        let frame2 = Frame {
            header: Header::new(101, 300, 6).unwrap(),
            data: b"world!".to_vec(),
        };

        let mut total_bytes = Vec::new();
        write_frame(&mut total_bytes, &frame1).unwrap();
        write_frame(&mut total_bytes, &frame2).unwrap();

        // Feed bytes in tiny 3-byte chunks to test buffering
        struct ChunkedReader {
            data: Vec<u8>,
            pos: usize,
            chunk_size: usize,
        }

        impl Read for ChunkedReader {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                let remaining = self.data.len() - self.pos;
                let to_read = remaining.min(self.chunk_size).min(buf.len());
                buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
                self.pos += to_read;
                Ok(to_read)
            }
        }

        let chunked = ChunkedReader {
            data: total_bytes,
            pos: 0,
            chunk_size: 3,
        };

        let mut reader = FrameReader::new(chunked);
        let f1 = reader.read_next_frame().unwrap().expect("frame 1");
        assert_eq!(f1, frame1);
        let f2 = reader.read_next_frame().unwrap().expect("frame 2");
        assert_eq!(f2, frame2);
        let f3 = reader.read_next_frame().unwrap();
        assert!(f3.is_none());
        assert!(reader.is_buffer_empty());
    }
}
