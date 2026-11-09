//! TTHeader streaming framing, compatible with Kitex v0.16.4.
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::streaming::{IoPhase, Metadata, StreamConfig, StreamError};

pub const MAGIC: u16 = 0x1000;
pub const STREAMING_FLAG: u16 = 0x0002;
pub const FRAME_TYPE_KEY: u16 = 27;
pub const METHOD_KEY: u16 = 9;
pub const THRIFT_STRUCT: u8 = 0x10;
pub const FIXED_HEADER_LEN: usize = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Meta = 1,
    Header = 2,
    Data = 3,
    Trailer = 4,
    Reset = 5,
}

impl FrameType {
    fn value(self) -> Bytes {
        Bytes::from_static(match self {
            Self::Meta => b"1",
            Self::Header => b"2",
            Self::Data => b"3",
            Self::Trailer => b"4",
            Self::Reset => b"5",
        })
    }

    fn parse(value: &[u8]) -> Result<Self, StreamError> {
        match value {
            b"1" => Ok(Self::Meta),
            b"2" => Ok(Self::Header),
            b"3" => Ok(Self::Data),
            b"4" => Ok(Self::Trailer),
            b"5" => Ok(Self::Reset),
            _ => Err(protocol("unknown frame type")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub stream_id: i32,
    pub kind: FrameType,
    pub method: Bytes,
    pub metadata: Metadata,
    pub payload: Bytes,
}

impl Frame {
    pub fn new(stream_id: i32, kind: FrameType, method: Bytes, payload: Bytes) -> Self {
        Self {
            stream_id,
            kind,
            method,
            metadata: Metadata::default(),
            payload,
        }
    }

    pub fn encode(&self, config: &StreamConfig) -> Result<Bytes, StreamError> {
        let mut metadata = self.metadata.clone();
        metadata.int.insert(FRAME_TYPE_KEY, self.kind.value());
        metadata.int.insert(METHOD_KEY, self.method.clone());
        let template = FrameTemplate::new(&metadata, config)?;
        template.encode(self.stream_id, &self.payload, config)
    }

    pub fn decode(mut bytes: Bytes, config: &StreamConfig) -> Result<Self, StreamError> {
        if bytes.len() < FIXED_HEADER_LEN {
            return Err(protocol("truncated fixed header"));
        }
        let total = bytes.get_u32() as usize;
        if total != bytes.len() || total + 4 > config.max_frame_bytes {
            return Err(protocol("invalid frame length"));
        }
        if bytes.get_u16() != MAGIC || bytes.get_u16() & STREAMING_FLAG == 0 {
            return Err(protocol("connection switched away from TTStream"));
        }
        let stream_id = bytes.get_i32();
        let header_len = usize::from(bytes.get_u16()) * 4;
        if header_len < 2 || header_len > config.max_header_bytes || header_len > bytes.len() {
            return Err(protocol("invalid header length"));
        }
        let mut header = bytes.split_to(header_len);
        let protocol_id = header.get_u8();
        if !matches!(protocol_id, 0 | 3 | 4 | THRIFT_STRUCT | 0x11) {
            return Err(protocol("unknown protocol id"));
        }
        let transforms = usize::from(header.get_u8());
        take(&mut header, transforms)?;
        let mut metadata = Metadata::default();
        let mut entries = 0usize;
        while header.has_remaining() {
            match header.get_u8() {
                0 => {}
                1 => {
                    let count = number(&mut header)?;
                    entries += count;
                    if entries > config.max_metadata_entries {
                        return Err(protocol("metadata entry limit exceeded"));
                    }
                    for _ in 0..count {
                        let key = string(&mut header)?;
                        let value = string(&mut header)?;
                        metadata.string.insert(key, value);
                    }
                }
                0x10 => {
                    let count = number(&mut header)?;
                    entries += count;
                    if entries > config.max_metadata_entries {
                        return Err(protocol("metadata entry limit exceeded"));
                    }
                    for _ in 0..count {
                        let key = number(&mut header)? as u16;
                        let value = string(&mut header)?;
                        metadata.int.insert(key, value);
                    }
                }
                0x11 => {
                    entries += 1;
                    if entries > config.max_metadata_entries {
                        return Err(protocol("metadata entry limit exceeded"));
                    }
                    metadata.insert(
                        Bytes::from_static(b"RPC_TRANSIT_gdpr-token"),
                        string(&mut header)?,
                    );
                }
                _ => return Err(protocol("unknown header info id")),
            }
        }
        let kind = FrameType::parse(
            metadata
                .int
                .get(&FRAME_TYPE_KEY)
                .ok_or_else(|| protocol("missing frame type"))?,
        )?;
        let method = metadata.int.get(&METHOD_KEY).cloned().unwrap_or_default();
        // Header and payload have independent compact allocations: no small slice keeps a
        // large network buffer alive after this frame has left the connection read loop.
        for value in metadata.int.values_mut() {
            *value = Bytes::copy_from_slice(value);
        }
        metadata.string = metadata
            .string
            .into_iter()
            .map(|(k, v)| (Bytes::copy_from_slice(&k), Bytes::copy_from_slice(&v)))
            .collect();
        Ok(Self {
            stream_id,
            kind,
            method: Bytes::copy_from_slice(&method),
            metadata,
            payload: Bytes::copy_from_slice(&bytes),
        })
    }
}

/// Reusable immutable header: DATA sends fill only length and signed stream id.
#[derive(Clone, Debug)]
pub struct FrameTemplate {
    header: Bytes,
}

impl FrameTemplate {
    pub fn data(method: Bytes, config: &StreamConfig) -> Result<Self, StreamError> {
        let mut metadata = Metadata::default();
        metadata.int.insert(FRAME_TYPE_KEY, FrameType::Data.value());
        metadata.int.insert(METHOD_KEY, method);
        Self::new(&metadata, config)
    }

    pub fn new(metadata: &Metadata, config: &StreamConfig) -> Result<Self, StreamError> {
        if metadata.int.len() + metadata.string.len() > config.max_metadata_entries {
            return Err(protocol("metadata entry limit exceeded"));
        }
        let mut header = BytesMut::new();
        header.put_u8(THRIFT_STRUCT);
        header.put_u8(0);
        if !metadata.string.is_empty() {
            header.put_u8(1);
            put_number(&mut header, metadata.string.len())?;
            for (key, value) in &metadata.string {
                put_string(&mut header, key)?;
                put_string(&mut header, value)?;
            }
        }
        if !metadata.int.is_empty() {
            header.put_u8(0x10);
            put_number(&mut header, metadata.int.len())?;
            for (key, value) in &metadata.int {
                header.put_u16(*key);
                put_string(&mut header, value)?;
            }
        }
        while header.len() % 4 != 0 {
            header.put_u8(0);
        }
        if header.len() > config.max_header_bytes || header.len() / 4 > u16::MAX as usize {
            return Err(protocol("header too large"));
        }
        Ok(Self {
            header: header.freeze(),
        })
    }

    pub(crate) fn payload_limit(&self, config: &StreamConfig) -> usize {
        config
            .max_frame_bytes
            .saturating_sub(FIXED_HEADER_LEN + self.header.len())
    }
    pub fn encode(
        &self,
        stream_id: i32,
        payload: &[u8],
        config: &StreamConfig,
    ) -> Result<Bytes, StreamError> {
        let len = FIXED_HEADER_LEN
            .checked_add(self.header.len())
            .and_then(|n| n.checked_add(payload.len()))
            .ok_or_else(|| protocol("frame length overflow"))?;
        if len > config.max_frame_bytes || len - 4 > u32::MAX as usize {
            return Err(protocol("frame too large"));
        }
        let mut output = BytesMut::with_capacity(len);
        output.put_u32((len - 4) as u32);
        output.put_u16(MAGIC);
        output.put_u16(STREAMING_FLAG);
        output.put_i32(stream_id);
        output.put_u16((self.header.len() / 4) as u16);
        output.extend_from_slice(&self.header);
        output.extend_from_slice(payload);
        Ok(output.freeze())
    }
}

pub fn matches_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 8
        && bytes[4..6] == MAGIC.to_be_bytes()
        && u16::from_be_bytes([bytes[6], bytes[7]]) & STREAMING_FLAG != 0
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    config: &StreamConfig,
) -> Result<Option<Frame>, StreamError> {
    match read_frame_admitted(reader, config, |_, _| Some(())).await? {
        Some(FrameRead::Frame(frame, ())) => Ok(Some(frame)),
        None => Ok(None),
        Some(FrameRead::Discarded) => unreachable!("unconditional admission"),
    }
}

pub(crate) enum FrameRead<T> {
    Frame(Frame, T),
    Discarded,
}

/// Decode only the bounded header before reserving payload memory. Rejected,
/// otherwise well-formed payloads are drained through an 8 KiB scratch buffer.
pub(crate) async fn read_frame_admitted<R: AsyncRead + Unpin, T>(
    reader: &mut R,
    config: &StreamConfig,
    admit: impl FnOnce(&Frame, usize) -> Option<T>,
) -> Result<Option<FrameRead<T>>, StreamError> {
    let mut fixed = [0u8; FIXED_HEADER_LEN];
    if reader
        .read(&mut fixed[..1])
        .await
        .map_err(|e| StreamError::transport(e, IoPhase::Read, true))?
        == 0
    {
        return Ok(None);
    }
    reader
        .read_exact(&mut fixed[1..])
        .await
        .map_err(|e| StreamError::transport(e, IoPhase::Read, true))?;
    let len = u32::from_be_bytes(fixed[..4].try_into().expect("fixed length")) as usize;
    if len < FIXED_HEADER_LEN - 4
        || len
            .checked_add(4)
            .is_none_or(|n| n > config.max_frame_bytes)
    {
        return Err(protocol("invalid frame length"));
    }
    if !matches_prefix(&fixed) {
        return Err(protocol("connection switched away from TTStream"));
    }
    let header_len = usize::from(u16::from_be_bytes([fixed[12], fixed[13]])) * 4;
    if header_len < 2
        || header_len > config.max_header_bytes
        || header_len > len - (FIXED_HEADER_LEN - 4)
    {
        return Err(protocol("invalid header length"));
    }
    let payload_len = len - (FIXED_HEADER_LEN - 4) - header_len;
    let mut header = BytesMut::with_capacity(FIXED_HEADER_LEN + header_len);
    // Frame::decode validates a self-contained header without retaining its backing buffer.
    fixed[..4].copy_from_slice(&((FIXED_HEADER_LEN - 4 + header_len) as u32).to_be_bytes());
    header.extend_from_slice(&fixed);
    header.resize(FIXED_HEADER_LEN + header_len, 0);
    reader
        .read_exact(&mut header[FIXED_HEADER_LEN..])
        .await
        .map_err(|e| StreamError::transport(e, IoPhase::Read, true))?;
    let mut frame = Frame::decode(header.freeze(), config)?;
    let Some(reservation) = admit(&frame, payload_len) else {
        let mut remaining = payload_len;
        let mut scratch = [0u8; 8192];
        while remaining != 0 {
            let len = remaining.min(scratch.len());
            reader
                .read_exact(&mut scratch[..len])
                .await
                .map_err(|e| StreamError::transport(e, IoPhase::Read, true))?;
            remaining -= len;
        }
        return Ok(Some(FrameRead::Discarded));
    };
    // Each payload owns exactly its allocation: no small slice pins a large receive buffer.
    let mut payload = vec![0; payload_len];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|e| StreamError::transport(e, IoPhase::Read, true))?;
    frame.payload = Bytes::from(payload);
    Ok(Some(FrameRead::Frame(frame, reservation)))
}

fn protocol(message: &str) -> StreamError {
    StreamError::Protocol(message.into())
}
fn number(bytes: &mut Bytes) -> Result<usize, StreamError> {
    if bytes.len() < 2 {
        return Err(protocol("truncated header number"));
    }
    Ok(usize::from(bytes.get_u16()))
}
fn take(bytes: &mut Bytes, len: usize) -> Result<Bytes, StreamError> {
    if len > bytes.len() {
        return Err(protocol("truncated header value"));
    }
    Ok(bytes.split_to(len))
}
fn string(bytes: &mut Bytes) -> Result<Bytes, StreamError> {
    let len = number(bytes)?;
    take(bytes, len)
}
fn put_number(bytes: &mut BytesMut, len: usize) -> Result<(), StreamError> {
    bytes.put_u16(u16::try_from(len).map_err(|_| protocol("header value too large"))?);
    Ok(())
}
fn put_string(bytes: &mut BytesMut, value: &[u8]) -> Result<(), StreamError> {
    put_number(bytes, value.len())?;
    bytes.extend_from_slice(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_ids_all_frame_types_and_binary_metadata() {
        let cfg = StreamConfig::default();
        for id in [0, -1, i32::MIN, i32::MAX, 1] {
            for kind in [
                FrameType::Meta,
                FrameType::Header,
                FrameType::Data,
                FrameType::Trailer,
                FrameType::Reset,
            ] {
                let mut frame = Frame::new(
                    id,
                    kind,
                    Bytes::from_static(b"chat"),
                    Bytes::from_static(b"\0\xff"),
                );
                frame
                    .metadata
                    .insert(Bytes::from_static(b"\xff"), Bytes::from_static(b"\0\x80"));
                let decoded = Frame::decode(frame.encode(&cfg).unwrap(), &cfg).unwrap();
                assert_eq!(decoded.stream_id, id);
                assert_eq!(decoded.kind, kind);
                assert_eq!(decoded.payload, frame.payload);
                assert_eq!(decoded.metadata.get("missing"), None);
                assert_eq!(decoded.metadata.string, frame.metadata.string);
            }
        }
    }
    #[test]
    fn every_truncation_and_length_overflow_is_rejected() {
        let cfg = StreamConfig::default();
        let bytes = Frame::new(
            -1,
            FrameType::Header,
            Bytes::from_static(b"chat"),
            Bytes::new(),
        )
        .encode(&cfg)
        .unwrap();
        for len in 0..bytes.len() {
            assert!(Frame::decode(bytes.slice(..len), &cfg).is_err());
        }
        let mut invalid = bytes.to_vec();
        invalid[12..14].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(Frame::decode(invalid.into(), &cfg).is_err());
    }
    #[test]
    fn bounded_seeded_mutations_never_panic_and_accepted_frames_roundtrip() {
        let config = StreamConfig {
            max_frame_bytes: 4096,
            max_header_bytes: 512,
            max_metadata_entries: 32,
            ..Default::default()
        };
        let seed = Frame::new(
            -1,
            FrameType::Header,
            Bytes::from_static(b"chat"),
            Bytes::from_static(b"payload"),
        )
        .encode(&config)
        .unwrap();
        let mut rng = 0x9e3779b97f4a7c15u64;
        let mut random = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for index in 0..10000 {
            let mut bytes = if index % 2 == 0 {
                seed.to_vec()
            } else {
                vec![0; (random() % 512) as usize]
            };
            if index % 2 == 0 {
                for _ in 0..1 + random() % 8 {
                    let offset = (random() as usize) % bytes.len();
                    bytes[offset] = random() as u8;
                }
            } else {
                for byte in &mut bytes {
                    *byte = random() as u8;
                }
            }
            if let Ok(mut frame) = Frame::decode(bytes.into(), &config) {
                // Encoder canonicalizes the two reserved fields; omitted method
                // is semantically empty and a numeric frame type is normalized.
                let encoded = frame.encode(&config).unwrap();
                frame.metadata.int.insert(9, frame.method.clone());
                frame
                    .metadata
                    .int
                    .insert(27, Bytes::from((frame.kind as u8).to_string()));
                assert_eq!(Frame::decode(encoded, &config).unwrap(), frame);
            }
        }
    }

    #[test]
    fn only_magic_and_flag_select_streaming() {
        let mut prefix = [0; 8];
        prefix[4..6].copy_from_slice(&MAGIC.to_be_bytes());
        assert!(!matches_prefix(&prefix));
        prefix[7] = 2;
        assert!(matches_prefix(&prefix));
        prefix[4] = 0;
        assert!(!matches_prefix(&prefix));
    }
}
