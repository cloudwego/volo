//! TTHeader streaming framing, compatible with Kitex v0.16.4.
use std::{collections::VecDeque, io::IoSlice};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use linkedbytes::{LinkedBytes, Node};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::streaming::{IoPhase, Metadata, StreamConfig, StreamError};

pub const MAGIC: u16 = 0x1000;
pub const STREAMING_FLAG: u16 = 0x0002;
pub const FRAME_TYPE_KEY: u16 = 27;
pub const METHOD_KEY: u16 = 9;
pub const THRIFT_STRUCT: u8 = 0x10;
pub const FIXED_HEADER_LEN: usize = 14;

/// Owned wire segments with a cursor that survives partial writes. Small typed
/// messages stay contiguous; Bytes/FastStr fields retain their original storage.
pub(crate) enum FrameBytes {
    Contiguous(Bytes),
    Raw(bytes::buf::Chain<Bytes, Bytes>),
    Segmented {
        nodes: VecDeque<Node>,
        offset: usize,
        remaining: usize,
    },
}

impl From<Bytes> for FrameBytes {
    fn from(bytes: Bytes) -> Self {
        Self::Contiguous(bytes)
    }
}
impl From<LinkedBytes> for FrameBytes {
    fn from(bytes: LinkedBytes) -> Self {
        match bytes.try_into_bytes_mut() {
            Ok(bytes) => Self::Contiguous(bytes.freeze()),
            Err(bytes) => {
                let remaining = bytes.len();
                // Collect the owned VecDeque iterator without converting/copying
                // its nodes. Only a partial first node needs an offset.
                let mut nodes: VecDeque<_> = bytes.into_iter_list().collect();
                while nodes.front().is_some_and(|node| node.as_ref().is_empty()) {
                    nodes.pop_front();
                }
                Self::Segmented {
                    nodes,
                    offset: 0,
                    remaining,
                }
            }
        }
    }
}
impl FrameBytes {
    pub(crate) fn len(&self) -> usize {
        self.remaining()
    }
    pub(crate) fn is_empty(&self) -> bool {
        !self.has_remaining()
    }
    #[cfg(all(test, feature = "ttstream"))]
    pub(crate) fn into_bytes(mut self) -> Bytes {
        let len = self.remaining();
        self.copy_to_bytes(len)
    }
}
impl Buf for FrameBytes {
    fn remaining(&self) -> usize {
        match self {
            Self::Contiguous(bytes) => bytes.len(),
            Self::Raw(bytes) => bytes.remaining(),
            Self::Segmented { remaining, .. } => *remaining,
        }
    }
    fn chunk(&self) -> &[u8] {
        match self {
            Self::Contiguous(bytes) => bytes.chunk(),
            Self::Raw(bytes) => bytes.chunk(),
            Self::Segmented { nodes, offset, .. } => {
                nodes.front().map_or(&[], |node| &node.as_ref()[*offset..])
            }
        }
    }
    fn advance(&mut self, mut count: usize) {
        assert!(count <= self.remaining(), "advancing past frame end");
        match self {
            Self::Contiguous(bytes) => bytes.advance(count),
            Self::Raw(bytes) => bytes.advance(count),
            Self::Segmented {
                nodes,
                offset,
                remaining,
            } => {
                *remaining -= count;
                while let Some(front) = nodes.front() {
                    let available = front.as_ref().len() - *offset;
                    if count < available {
                        *offset += count;
                        break;
                    }
                    count -= available;
                    *offset = 0;
                    nodes.pop_front();
                }
            }
        }
    }
    fn chunks_vectored<'a>(&'a self, dst: &mut [IoSlice<'a>]) -> usize {
        match self {
            Self::Contiguous(bytes) => bytes.chunks_vectored(dst),
            Self::Raw(bytes) => bytes.chunks_vectored(dst),
            Self::Segmented { nodes, offset, .. } => {
                let mut count = 0;
                for (index, node) in nodes.iter().enumerate() {
                    if count == dst.len() {
                        break;
                    }
                    let bytes = &node.as_ref()[if index == 0 { *offset } else { 0 }..];
                    if !bytes.is_empty() {
                        dst[count] = IoSlice::new(bytes);
                        count += 1;
                    }
                }
                count
            }
        }
    }
}

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

    pub fn decode(bytes: Bytes, config: &StreamConfig) -> Result<Self, StreamError> {
        Self::decode_inner(bytes, config, false)
    }

    fn decode_inner(
        mut bytes: Bytes,
        config: &StreamConfig,
        compact_header: bool,
    ) -> Result<Self, StreamError> {
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
        let header = bytes.split_to(header_len);
        let mut header = if compact_header {
            header
        } else {
            Bytes::copy_from_slice(&header)
        };
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
        Ok(Self {
            stream_id,
            kind,
            method,
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
        self.encode_with(stream_id, payload.len(), config, |output| {
            output.extend_from_slice(payload);
            Ok(())
        })
    }

    pub(crate) fn encode_with(
        &self,
        stream_id: i32,
        payload_len: usize,
        config: &StreamConfig,
        encode: impl FnOnce(&mut BytesMut) -> Result<(), StreamError>,
    ) -> Result<Bytes, StreamError> {
        let len = self.frame_len(payload_len, config)?;
        let mut output = BytesMut::with_capacity(len);
        self.write_header(stream_id, len, &mut output);
        encode(&mut output)?;
        if output.len() != len {
            return Err(protocol("encoded size disagrees with reserved length"));
        }
        Ok(output.freeze())
    }

    pub(crate) fn encode_linked(
        &self,
        stream_id: i32,
        payload_len: usize,
        zero_copy_len: usize,
        config: &StreamConfig,
        encode: impl FnOnce(&mut LinkedBytes) -> Result<(), StreamError>,
    ) -> Result<FrameBytes, StreamError> {
        let len = self.frame_len(payload_len, config)?;
        let mut output = LinkedBytes::with_capacity(len - zero_copy_len.min(payload_len));
        self.write_header(stream_id, len, &mut output);
        encode(&mut output)?;
        if output.len() != len {
            return Err(protocol("encoded size disagrees with reserved length"));
        }
        Ok(output.into())
    }

    pub(crate) fn encode_bytes(
        &self,
        stream_id: i32,
        payload: Bytes,
        config: &StreamConfig,
    ) -> Result<FrameBytes, StreamError> {
        let len = self.frame_len(payload.len(), config)?;
        let mut header = BytesMut::with_capacity(len - payload.len());
        self.write_header(stream_id, len, &mut header);
        // Raw sends need only two segments, without allocating a node list.
        Ok(FrameBytes::Raw(header.freeze().chain(payload)))
    }

    fn frame_len(&self, payload_len: usize, config: &StreamConfig) -> Result<usize, StreamError> {
        let len = FIXED_HEADER_LEN
            .checked_add(self.header.len())
            .and_then(|n| n.checked_add(payload_len))
            .ok_or_else(|| protocol("frame length overflow"))?;
        if len > config.max_frame_bytes || len - 4 > u32::MAX as usize {
            return Err(protocol("frame too large"));
        }
        Ok(len)
    }

    fn write_header(&self, stream_id: i32, len: usize, output: &mut impl BufMut) {
        output.put_u32((len - 4) as u32);
        output.put_u16(MAGIC);
        output.put_u16(STREAMING_FLAG);
        output.put_i32(stream_id);
        output.put_u16((self.header.len() / 4) as u16);
        output.put_slice(&self.header);
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
    let mut header = BytesMut::new();
    match read_frame_admitted(reader, config, &mut header, false, |_, _| Some(())).await? {
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
    header: &mut BytesMut,
    skip_data_metadata: bool,
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
    header.clear();
    header.reserve(FIXED_HEADER_LEN + header_len);
    fixed[..4].copy_from_slice(&((FIXED_HEADER_LEN - 4 + header_len) as u32).to_be_bytes());
    header.extend_from_slice(&fixed);
    let mut limited = (&mut *reader).take(header_len as u64);
    while header.len() < FIXED_HEADER_LEN + header_len {
        if limited
            .read_buf(header)
            .await
            .map_err(|e| StreamError::transport(e, IoPhase::Read, true))?
            == 0
        {
            return Err(protocol("truncated TTStream header"));
        }
    }
    let mut frame = if skip_data_metadata && plain_data_header(&header[FIXED_HEADER_LEN..], config)?
    {
        Frame::new(
            i32::from_be_bytes(fixed[8..12].try_into().expect("stream id")),
            FrameType::Data,
            Bytes::new(),
            Bytes::new(),
        )
    } else {
        // Only control/metadata frames retain header storage. DATA reuses it.
        Frame::decode_inner(Bytes::copy_from_slice(header), config, true)?
    };
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
    let mut payload = BytesMut::with_capacity(payload_len);
    let mut limited = reader.take(payload_len as u64);
    while payload.len() < payload_len {
        if limited
            .read_buf(&mut payload)
            .await
            .map_err(|e| StreamError::transport(e, IoPhase::Read, true))?
            == 0
        {
            return Err(StreamError::transport(
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated TTStream payload",
                ),
                IoPhase::Read,
                true,
            ));
        }
    }
    frame.payload = payload.freeze();
    Ok(Some(FrameRead::Frame(frame, reservation)))
}

/// Validate the usual DATA header without allocating maps or reference-counted
/// slices. Unusual headers fall back to the full decoder with identical limits.
fn plain_data_header(mut header: &[u8], config: &StreamConfig) -> Result<bool, StreamError> {
    if header.len() < 2 || !matches!(header[0], 0 | 3 | 4 | THRIFT_STRUCT | 0x11) {
        return Ok(false);
    }
    let transforms = usize::from(header[1]);
    header = match header.get(2 + transforms..) {
        Some(header) => header,
        None => return Ok(false),
    };
    let mut entries = 0usize;
    let mut kind = None;
    while let Some((&info, rest)) = header.split_first() {
        header = rest;
        match info {
            0 => {}
            0x10 => {
                if header.len() < 2 {
                    return Ok(false);
                }
                let count = usize::from(header.get_u16());
                entries += count;
                if entries > config.max_metadata_entries {
                    return Err(protocol("metadata entry limit exceeded"));
                }
                for _ in 0..count {
                    if header.len() < 4 {
                        return Ok(false);
                    }
                    let key = header.get_u16();
                    let len = usize::from(header.get_u16());
                    if len > header.len() {
                        return Ok(false);
                    }
                    let (value, rest) = header.split_at(len);
                    header = rest;
                    match key {
                        FRAME_TYPE_KEY => kind = Some(value),
                        METHOD_KEY => {}
                        _ => return Ok(false),
                    }
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(kind == Some(b"3".as_slice()))
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
    fn data_fast_path_never_accepts_headers_rejected_by_full_decoder() {
        let config = StreamConfig::default();
        let wire = Frame::new(
            7,
            FrameType::Data,
            Bytes::from_static(b"Echo"),
            Bytes::new(),
        )
        .encode(&config)
        .unwrap();
        assert!(plain_data_header(&wire[FIXED_HEADER_LEN..], &config).unwrap());
        for index in FIXED_HEADER_LEN..wire.len() {
            for byte in 0..=255 {
                let mut mutated = wire.to_vec();
                mutated[index] = byte;
                if plain_data_header(&mutated[FIXED_HEADER_LEN..], &config).unwrap_or(false) {
                    assert_eq!(
                        Frame::decode(mutated.into(), &config).unwrap().kind,
                        FrameType::Data
                    );
                }
            }
        }
        let limit = StreamConfig {
            max_metadata_entries: 1,
            ..config
        };
        assert!(plain_data_header(&wire[FIXED_HEADER_LEN..], &limit).is_err());
    }

    #[tokio::test]
    async fn connection_data_headers_reuse_storage_and_control_metadata_survives() {
        let config = StreamConfig::default();
        let data = Frame::new(
            7,
            FrameType::Data,
            Bytes::from_static(b"Echo"),
            Bytes::from_static(b"payload"),
        );
        let mut control = Frame::new(
            7,
            FrameType::Trailer,
            Bytes::from_static(b"Echo"),
            Bytes::new(),
        );
        control.metadata.insert("key", "value");
        let wire = [
            data.encode(&config).unwrap(),
            data.encode(&config).unwrap(),
            control.encode(&config).unwrap(),
        ]
        .concat();
        let mut reader = wire.as_slice();
        let mut header = BytesMut::with_capacity(128);
        let pointer = header.as_ptr();
        for kind in [FrameType::Data, FrameType::Data, FrameType::Trailer] {
            let Some(FrameRead::Frame(frame, ())) =
                read_frame_admitted(&mut reader, &config, &mut header, true, |_, _| Some(()))
                    .await
                    .unwrap()
            else {
                panic!("missing frame")
            };
            assert_eq!(frame.kind, kind);
            assert_eq!(header.as_ptr(), pointer);
            if kind == FrameType::Data {
                assert!(frame.metadata.int.is_empty());
                assert_eq!(frame.payload, data.payload);
            } else {
                assert_eq!(frame.metadata.get("key").unwrap(), "value");
            }
        }
    }

    #[test]
    fn segmented_frame_cursor_handles_short_writes_and_iovec_limits() {
        let config = StreamConfig::default();
        let template = FrameTemplate::data(Bytes::from_static(b"chat"), &config).unwrap();
        let chunks: Vec<_> = (0..100).map(|i| Bytes::from(vec![i; 257])).collect();
        let payload: Vec<_> = chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect();
        let expected = template.encode(-1, &payload, &config).unwrap();
        for slots in [1, 2, 64, 128] {
            for write_limit in [1, 3, 257, 4096, usize::MAX] {
                let mut frame = template
                    .encode_linked(-1, payload.len(), payload.len(), &config, |out| {
                        for chunk in &chunks {
                            out.insert(chunk.clone());
                        }
                        Ok(())
                    })
                    .unwrap();
                let mut actual = Vec::new();
                while frame.has_remaining() {
                    let mut slices = vec![IoSlice::new(&[]); slots];
                    let count = frame.chunks_vectored(&mut slices);
                    assert!(count > 0);
                    let mut available = write_limit;
                    let start = actual.len();
                    for slice in &slices[..count] {
                        let len = slice.len().min(available);
                        actual.extend_from_slice(&slice[..len]);
                        available -= len;
                    }
                    frame.advance(actual.len() - start);
                    assert_eq!(frame.remaining(), expected.len() - actual.len());
                }
                assert!(frame.chunk().is_empty());
                assert_eq!(actual, expected);
            }
        }
    }

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
