#![allow(dead_code)]

//! TTheader is a transport protocol designed by CloudWeGo.
//!
//! For more information, please visit https://www.cloudwego.io/docs/kitex/reference/transport_protocol_ttheader/

use std::{
    collections::HashMap, convert::TryFrom, default::Default, net::SocketAddr, str::from_utf8,
    sync::Arc, time::Duration,
};

use bytes::{Buf, BufMut, BytesMut};
use linkedbytes::LinkedBytes;
use metainfo::{Backward, Forward};
use num_enum::TryFromPrimitive;
use pilota::thrift::{new_protocol_error, ProtocolErrorKind, TransportErrorKind};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt};
use tracing::{trace, warn};
use volo::{
    context::{Endpoint, Role},
    util::buf_reader::BufReader,
};

use super::MakeZeroCopyCodec;
use crate::{
    codec::default::{ZeroCopyDecoder, ZeroCopyEncoder},
    context::{Config, ThriftContext},
    EntryMessage, ThriftMessage,
};

/// [`MakeTTHeaderCodec`] implements [`MakeZeroCopyCodec`] to create [`TTheaderEncoder`] and
/// [`TTHeaderDecoder`].
#[derive(Clone)]
pub struct MakeTTHeaderCodec<Inner: MakeZeroCopyCodec> {
    inner: Inner,
}

impl<Inner: MakeZeroCopyCodec> MakeTTHeaderCodec<Inner> {
    pub fn new(inner: Inner) -> Self {
        Self { inner }
    }
}

impl<Inner: MakeZeroCopyCodec> MakeZeroCopyCodec for MakeTTHeaderCodec<Inner> {
    type Encoder = TTHeaderEncoder<Inner::Encoder>;

    type Decoder = TTHeaderDecoder<Inner::Decoder>;

    fn make_codec(&self) -> (Self::Encoder, Self::Decoder) {
        let (encoder, decoder) = self.inner.make_codec();
        (TTHeaderEncoder::new(encoder), TTHeaderDecoder::new(decoder))
    }
}

/// This is used to tell the encoder to encode TTHeader at server side.
pub struct HasTTHeader(bool);

#[derive(Clone)]
pub struct TTHeaderDecoder<D: ZeroCopyDecoder> {
    inner: D,
}

impl<D: ZeroCopyDecoder> TTHeaderDecoder<D> {
    pub fn new(inner: D) -> Self {
        Self { inner }
    }
}

/// 4-bytes length + 2-bytes magic
/// https://www.cloudwego.io/docs/kitex/reference/transport_protocol_ttheader/
pub const HEADER_DETECT_LENGTH: usize = 6;

#[async_trait::async_trait]
impl<D> ZeroCopyDecoder for TTHeaderDecoder<D>
where
    D: ZeroCopyDecoder,
{
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        mut bytes: BytesMut,
    ) -> crate::Result<Option<ThriftMessage<Msg>>> {
        if bytes.len() < HEADER_DETECT_LENGTH {
            // not enough bytes to detect, must not be TTHeader, so just forward to inner
            return self.inner.decode(cx, bytes);
        }

        if is_ttheader(&bytes[..HEADER_DETECT_LENGTH]) {
            let _size = bytes.get_u32() as usize;
            // decode ttheader
            decode(cx, &mut bytes)?;
            // set has ttheader flag
            cx.extensions_mut().insert(HasTTHeader(true));
        }
        // decode inner
        self.inner.decode(cx, bytes)
    }

    async fn decode_async<
        Msg: Send + EntryMessage,
        Cx: ThriftContext,
        R: AsyncRead + Unpin + Send + Sync,
    >(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> crate::Result<Option<ThriftMessage<Msg>>> {
        // check if is ttheader
        if let Ok(buf) = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await {
            if is_ttheader(buf) {
                // read all the data out, and call inner decode instead of decode_async
                let size = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
                reader.consume(4);
                let mut bytes = BytesMut::with_capacity(size);
                unsafe {
                    bytes.set_len(size);
                }
                reader.read_exact(&mut bytes[..size]).await?;
                // decode ttheader
                decode(cx, &mut bytes)?;
                // set has ttheader flag
                cx.extensions_mut().insert(HasTTHeader(true));
                // decode inner
                self.inner.decode(cx, bytes)
            } else {
                // no TTHeader, just forward to inner decoder
                self.inner.decode_async(cx, reader).await
            }
        } else {
            return self.inner.decode_async(cx, reader).await;
        }
    }
}

// Checks if the first 6 bytes are a valid TTHeader.
pub fn is_ttheader(buf: &[u8]) -> bool {
    buf[4..6] == [0x10, 0x00]
}

#[derive(Clone)]
pub struct TTHeaderEncoder<E: ZeroCopyEncoder> {
    inner: E,
    inner_size: usize, // used to cache the size
}

impl<E: ZeroCopyEncoder> TTHeaderEncoder<E> {
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            inner_size: 0,
        }
    }
}

impl<E> ZeroCopyEncoder for TTHeaderEncoder<E>
where
    E: ZeroCopyEncoder,
{
    fn encode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        linked_bytes: &mut LinkedBytes,
        msg: ThriftMessage<Msg>,
    ) -> crate::Result<()> {
        let dst = linked_bytes.bytes_mut();
        // only encode ttheader if role is client or server has detected ttheader in decode
        if cx.rpc_info().role() == Role::Client
            || cx
                .extensions()
                .get::<HasTTHeader>()
                .unwrap_or(&HasTTHeader(false))
                .0
        {
            // encode ttheader first
            encode(cx, dst, self.inner_size)?;
        }
        self.inner.encode(cx, linked_bytes, msg)
    }

    fn size<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: &ThriftMessage<Msg>,
    ) -> crate::Result<usize> {
        self.inner_size = self.inner.size(cx, msg)?;
        // only calc ttheader size if role is client or server has detected ttheader in decode
        if cx.rpc_info().role() == Role::Client
            || cx
                .extensions()
                .get::<HasTTHeader>()
                .unwrap_or(&HasTTHeader(false))
                .0
        {
            Ok(self.inner_size + encode_size(cx)?)
        } else {
            Ok(self.inner_size)
        }
    }
}

pub const TT_HEADER_MAGIC: u16 = 0x1000;

mod info {
    pub const INFO_PADDING: u8 = 0x00;
    pub const INFO_KEY_VALUE: u8 = 0x01;
    pub const INFO_INT_KEY_VALUE: u8 = 0x10;
    pub const ACL_TOKEN_KEY_VALUE: u8 = 0x11;
}

// remote ip
pub(crate) const HEADER_TRANS_REMOTE_ADDR: &str = "rip";
// the connection peer will shutdown later, so it send back the header to tell client to close the
// connection.
pub(crate) const HEADER_CONNECTION_READY_TO_RESET: &str = "crrst";

#[derive(TryFromPrimitive, Clone, Copy)]
#[repr(u8)]
pub enum ProtocolId {
    Binary = 0,
    Compact = 2,   // Apache Thrift compact protocol
    CompactV2 = 3, // fbthrift compact protocol
    Protobuf = 4,
}

impl Default for ProtocolId {
    fn default() -> ProtocolId {
        ProtocolId::Binary
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy, TryFromPrimitive)]
#[repr(u16)]
pub enum IntMetaKey {
    FromService = 3,

    ToService = 6,
    ToMethod = 9,
    DestAddress = 11,

    // in ms
    RPCTimeout = 12,
    // in ms
    ConnTimeout = 17,
    // always set to 3
    WithHeader = 16,

    MsgType = 22,
}

/// TTHeader Protocol detailed: https://www.cloudwego.io/docs/kitex/reference/transport_protocol_ttheader/
/// +-------------2Byte--------------|-------------2Byte-------------+
/// +----------------------------------------------------------------+
/// | 0|                          LENGTH                             |
/// +----------------------------------------------------------------+
/// | 0|       HEADER MAGIC          |            FLAGS              |
/// +----------------------------------------------------------------+
/// |                         SEQUENCE NUMBER                        |
/// +----------------------------------------------------------------+
/// | 0|     Header Size(/32)        | ...
/// +---------------------------------
///
/// Header is of variable size:
/// (and starts at offset 14)
///
/// +----------------------------------------------------------------+
/// | PROTOCOL ID  |NUM TRANSFORMS . |TRANSFORM 0 ID (uint8)|
/// +----------------------------------------------------------------+
/// |  TRANSFORM 0 DATA ...
/// +----------------------------------------------------------------+
/// |         ...                              ...                   |
/// +----------------------------------------------------------------+
/// |        INFO 0 ID (uint8)      |       INFO 0  DATA ...
/// +----------------------------------------------------------------+
/// |         ...                              ...                   |
/// +----------------------------------------------------------------+
/// |                                                                |
/// |                              PAYLOAD                           |
/// |                                                                |
/// +----------------------------------------------------------------+
// if anyone changed this function, please make sure the encode_size is in sync
pub(crate) fn encode<Cx: ThriftContext>(
    cx: &mut Cx,
    dst: &mut BytesMut,
    size: usize,
) -> Result<(), crate::Error> {
    metainfo::METAINFO.with(|metainfo| {
        let metainfo = metainfo.borrow_mut();
        let zero_index = dst.len();
        // Alloc 4-byte space as length
        dst.reserve(4);
        unsafe {
            dst.advance_mut(4);
        }

        // tt header magic
        dst.put_u16(TT_HEADER_MAGIC);
        // flags
        dst.put_u16(0);
        let seq_id = cx.seq_id();
        dst.put_u32(seq_id as u32); // TODO: thrift seq_id is i32, tt header is u32?

        // Alloc 2-byte space as header length
        dst.reserve(2);
        unsafe {
            dst.advance_mut(2);
        }

        // protocol_id
        let protocol_id = cx
            .extensions()
            .get::<ProtocolId>()
            .unwrap_or(&ProtocolId::Binary);
        dst.put_u8(*protocol_id as u8);
        dst.put_u8(0); // TODO: item.transform_ids_num

        // TODO: item.transform_ids
        // if let Some(ids) = &item.transform_ids {
        //     dst.put_slice(ids);
        // }

        let role = cx.rpc_info().role();

        // Write string KV start.

        let has_string_kv = match role {
            Role::Client => {
                metainfo.get_all_persistents().is_some() || metainfo.get_all_transients().is_some()
            }
            Role::Server => {
                metainfo.get_all_backward_transients().is_some()
                    || cx.encode_conn_reset().unwrap_or(false)
            }
        };

        if has_string_kv {
            dst.put_u8(info::INFO_KEY_VALUE);
            let string_kv_index = dst.len();
            let mut string_kv_len = 0_u16;
            dst.reserve(2);
            unsafe {
                dst.advance_mut(2);
            }

            match role {
                Role::Client => {
                    if let Some(ap) = metainfo.get_all_persistents() {
                        for (key, value) in ap {
                            let key_len = metainfo::RPC_PREFIX_PERSISTENT.len() + key.len();
                            dst.put_u16(key_len as u16);
                            dst.put_slice(metainfo::RPC_PREFIX_PERSISTENT.as_bytes());
                            dst.put_slice(key.as_bytes());
                            dst.put_u16(value.len() as u16);
                            dst.put_slice(value.as_bytes());
                            string_kv_len += 1;
                        }
                    }
                    if let Some(at) = metainfo.get_all_transients() {
                        for (key, value) in at {
                            let key_len = metainfo::RPC_PREFIX_TRANSIENT.len() + key.len();
                            dst.put_u16(key_len as u16);
                            dst.put_slice(metainfo::RPC_PREFIX_TRANSIENT.as_bytes());
                            dst.put_slice(key.as_bytes());
                            dst.put_u16(value.len() as u16);
                            dst.put_slice(value.as_bytes());
                            string_kv_len += 1;
                        }
                    }
                }
                Role::Server => {
                    if let Some(at) = metainfo.get_all_backward_transients() {
                        for (key, value) in at {
                            let key_len = metainfo::RPC_PREFIX_BACKWARD.len() + key.len();
                            dst.put_u16(key_len as u16);
                            dst.put_slice(metainfo::RPC_PREFIX_BACKWARD.as_bytes());
                            dst.put_slice(key.as_bytes());
                            dst.put_u16(value.len() as u16);
                            dst.put_slice(value.as_bytes());
                            string_kv_len += 1;
                        }
                    }
                    if cx.encode_conn_reset().unwrap_or(false) {
                        dst.put_u16(5);
                        dst.put_slice("crrst".as_bytes());
                        dst.put_u16(1);
                        dst.put_slice("1".as_bytes());
                        string_kv_len += 1;
                    }
                }
            }

            let mut buf = &mut dst[string_kv_index..string_kv_index + 2];
            buf.put_u16(string_kv_len);
        }

        // Write int KV start.
        dst.put_u8(info::INFO_INT_KEY_VALUE);
        let int_kv_index = dst.len();
        let mut int_kv_len = 0_u16;
        dst.reserve(2);
        unsafe {
            dst.advance_mut(2);
        }

        match role {
            Role::Server => {
                let msg_type: u8 = cx.msg_type().into();
                dst.put_u16(IntMetaKey::MsgType as u16);
                dst.put_u16(1);
                dst.put_slice(&[msg_type]);
                int_kv_len += 1;
            }

            Role::Client => {
                // WithHeader
                dst.put_u16(IntMetaKey::WithHeader as u16);
                dst.put_u16(1);
                dst.put_slice("3".as_bytes());
                int_kv_len += 1;

                // Config
                if let Some(config) = cx.rpc_info().config.as_ref() {
                    if let Some(timeout) = config.rpc_timeout() {
                        let timeout = timeout.as_millis().to_string();
                        dst.put_u16(IntMetaKey::RPCTimeout as u16);
                        dst.put_u16(timeout.len() as u16);
                        dst.put_slice(timeout.as_bytes());
                        int_kv_len += 1;
                    }

                    if let Some(timeout) = config.connect_timeout() {
                        let timeout = timeout.as_millis().to_string();
                        dst.put_u16(IntMetaKey::ConnTimeout as u16);
                        dst.put_u16(timeout.len() as u16);
                        dst.put_slice(timeout.as_bytes());
                        int_kv_len += 1;
                    }
                }

                // Caller
                if let Some(caller) = cx.rpc_info().caller.as_ref() {
                    let svc = caller.service_name();
                    dst.put_u16(IntMetaKey::FromService as u16);
                    dst.put_u16(svc.len() as u16);
                    dst.put_slice(svc.as_bytes());
                    int_kv_len += 1;
                }

                // Callee
                if let Some(callee) = cx.rpc_info().callee.as_ref() {
                    let method = cx.rpc_info().method.as_ref().unwrap();
                    dst.put_u16(IntMetaKey::ToMethod as u16);
                    dst.put_u16(method.len() as u16);
                    dst.put_slice(method.as_bytes());
                    int_kv_len += 1;

                    let svc = callee.service_name();
                    dst.put_u16(IntMetaKey::ToService as u16);
                    dst.put_u16(svc.len() as u16);
                    dst.put_slice(svc.as_bytes());
                    int_kv_len += 1;

                    if let Some(addr) = callee.address() {
                        let addr = addr.to_string();
                        dst.put_u16(IntMetaKey::DestAddress as u16);
                        dst.put_u16(addr.len() as u16);
                        dst.put_slice(addr.as_bytes());
                        int_kv_len += 1;
                    }
                }
            }
        };

        // fill int kv length
        let mut buf = &mut dst[int_kv_index..int_kv_index + 2];
        buf.put_u16(int_kv_len);

        // write padding
        let overflow = (dst.len() - 14 - zero_index) % 4;
        let padding = (4 - overflow) % 4;
        (0..padding).for_each(|_| dst.put_u8(0));

        // fill header length
        let header_size = dst.len() - zero_index;
        let mut buf = &mut dst[zero_index + 12..zero_index + 12 + 2];
        let written_header_size = (header_size - 14) / 4;
        if written_header_size > u16::MAX as usize {
            return Err(crate::Error::Pilota(pilota::thrift::new_transport_error(
                TransportErrorKind::SizeLimit,
                format!("ttheader header size {} overflows u16", written_header_size),
            )));
        }
        buf.put_u16(written_header_size.try_into().unwrap());
        trace!(
            "[VOLO] encode ttheader write header size: {}",
            written_header_size
        );

        let size = header_size + size;

        // fill length
        let mut buf = &mut dst[zero_index..zero_index + 4];
        buf.put_u32((size - 4).try_into().unwrap());
        trace!("[VOLO] encode ttheader write length size: {}", size - 4);
        Ok(())
    })?;
    Ok(())
}

// this must be with sync to the encode impl
pub(crate) fn encode_size<Cx: ThriftContext>(cx: &mut Cx) -> Result<usize, crate::Error> {
    let thrift_cx = cx;
    Ok(metainfo::METAINFO.with(|metainfo| {
        let metainfo = metainfo.borrow_mut();
        let mut len = 0;
        // 4-byte space as length
        len += 4;

        // tt header magic
        len += 2;
        // flags
        len += 2;
        // seq id
        len += 4;

        // 2-byte space as header length
        len += 2;

        // protocol_id
        len += 1; // TODO: item.protocol_id as u8(0=Binary; 2=Compact)
        len += 1; // TODO: item.transform_ids_num

        // TODO: item.transform_ids
        // if let Some(ids) = &item.transform_ids {
        //     dst.put_slice(ids);
        // }

        let role = thrift_cx.rpc_info().role();

        // Write string KV start.

        let has_string_kv = match role {
            Role::Client => {
                metainfo.get_all_persistents().is_some() || metainfo.get_all_transients().is_some()
            }
            Role::Server => {
                metainfo.get_all_backward_transients().is_some()
                    || thrift_cx.encode_conn_reset().unwrap_or(false)
            }
        };

        if has_string_kv {
            // info key value
            len += 1;
            // string kv len
            len += 2;

            match role {
                Role::Client => {
                    if let Some(ap) = metainfo.get_all_persistents() {
                        for (key, value) in ap {
                            let key_len = metainfo::RPC_PREFIX_PERSISTENT.len() + key.len();
                            len += 2;
                            len += key_len;
                            len += 2;
                            len += value.as_bytes().len();
                        }
                    }
                    if let Some(at) = metainfo.get_all_transients() {
                        for (key, value) in at {
                            let key_len = metainfo::RPC_PREFIX_TRANSIENT.len() + key.len();
                            len += 2;
                            len += key_len;
                            len += 2;
                            len += value.as_bytes().len();
                        }
                    }
                }
                Role::Server => {
                    if let Some(at) = metainfo.get_all_backward_transients() {
                        for (key, value) in at {
                            let key_len = metainfo::RPC_PREFIX_BACKWARD.len() + key.len();
                            len += 2;
                            len += key_len;
                            len += 2;
                            len += value.as_bytes().len();
                        }
                    }
                    if thrift_cx.encode_conn_reset().unwrap_or(false) {
                        len += 2;
                        len += "crrst".as_bytes().len();
                        len += 2;
                        len += "1".as_bytes().len();
                    }
                }
            }
        }

        // int KV start
        len += 1;
        // int kv length
        len += 2;

        match role {
            Role::Server => {
                let msg_type: u8 = thrift_cx.msg_type().into();
                len += 2;
                len += 2;
                len += &[msg_type].len();
            }

            Role::Client => {
                // WithHeader
                len += 2;
                len += 2;
                len += "3".as_bytes().len();

                // Config
                if let Some(config) = thrift_cx.rpc_info().config.as_ref() {
                    if let Some(timeout) = config.rpc_timeout() {
                        let timeout = timeout.as_millis().to_string();
                        len += 2;
                        len += 2;
                        len += timeout.as_bytes().len();
                    }

                    if let Some(timeout) = config.connect_timeout() {
                        let timeout = timeout.as_millis().to_string();
                        len += 2;
                        len += 2;
                        len += timeout.as_bytes().len();
                    }
                }

                // Caller
                if let Some(caller) = thrift_cx.rpc_info().caller.as_ref() {
                    let svc = caller.service_name();
                    len += 2;
                    len += 2;
                    len += svc.as_bytes().len();
                }

                // Callee
                if let Some(callee) = thrift_cx.rpc_info().callee.as_ref() {
                    let method = thrift_cx.rpc_info().method.as_ref().unwrap();
                    len += 2;
                    len += 2;
                    len += method.as_bytes().len();

                    let svc = callee.service_name();
                    len += 2;
                    len += 2;
                    len += svc.as_bytes().len();

                    if let Some(addr) = callee.address() {
                        let addr = addr.to_string();
                        len += 2;
                        len += 2;
                        len += addr.as_bytes().len();
                    }
                }
            }
        };

        // write padding
        let overflow = (len - 14) % 4;
        let padding = (4 - overflow) % 4;
        len += padding;
        len
    }))
}

pub(crate) fn decode<Cx: ThriftContext>(
    cx: &mut Cx,
    src: &mut BytesMut,
) -> Result<(), crate::Error> {
    metainfo::METAINFO.with(|metainfo| {
            let metainfo = &mut *metainfo.borrow_mut();
            let _magic = src.get_u16();
            let _flags = src.get_u16();
            let _sequence_id = src.get_u32(); // TODO: seq id should be i32?
            let header_size = src.get_u16();
            let protocol_id = src.get_u8();
            if let Ok(protocol_id) = ProtocolId::try_from_primitive(protocol_id) {
                cx.extensions_mut().insert(protocol_id);
            } else {
                return Err(
                    crate::Error::Pilota(
                        pilota::thrift::new_protocol_error(
                            ProtocolErrorKind::BadVersion,
                            format!("unknown protocol id: {} in ttheader", protocol_id)
                        )
                    )
                );
            }

            let transform_ids_num = src.get_u8();
            let mut _transform_ids = None;
            if transform_ids_num > 0 {
                let mut _transform_ids_inner = vec![0u8; transform_ids_num as usize];
                src.copy_to_slice(&mut _transform_ids_inner);
                _transform_ids = Some(_transform_ids_inner);
            }
            let mut headers = HashMap::new();
            let mut int_headers = HashMap::new();
            let mut _padding_num = 0usize;

            let mut remaining_header_size = (header_size as usize) * 4 - 2 /* protocol_id and transform_ids_num */ - transform_ids_num as usize;

            while remaining_header_size > 0 {
                remaining_header_size -= 1;
                let info_id = src.get_u8();
                match info_id {
                    info::INFO_PADDING => {
                        _padding_num += 1;
                        continue;
                    }
                    info::INFO_KEY_VALUE => {
                        remaining_header_size -= 2;
                        let kv_size = src.get_u16();
                        headers.reserve(kv_size as usize);
                        for _ in 0..kv_size {
                            remaining_header_size -= 2;
                            let key_len = src.get_u16();
                            remaining_header_size -= key_len as usize;
                            let mut key = vec![0u8; key_len as usize];
                            src.copy_to_slice(&mut key);
                            remaining_header_size -= 2;
                            let value_len = src.get_u16();
                            remaining_header_size -= value_len as usize;
                            let mut value = vec![0u8; value_len as usize];
                            src.copy_to_slice(&mut value);
                            headers.insert(
                                from_utf8(&key)
                                    .map_err(|e| {
                                        new_protocol_error(
                                            ProtocolErrorKind::InvalidData,
                                            format!(
                                                "invalid header key which is not utf-8 {:?}: {}",
                                                key, e
                                            ),
                            )})?
                                    .to_string(),
                                from_utf8(&value)
                                    .map_err(|e| {
                                        new_protocol_error(
                                            ProtocolErrorKind::InvalidData,
                                            format!(
                                                "invalid header value which is not utf-8 {:?}: {}",
                                                key, e
                                            ),
                                        )
                                    })?
                                    .to_string(),
                            );
                        }
                    }
                    info::INFO_INT_KEY_VALUE => {
                        remaining_header_size -= 2;
                        let kv_size = src.get_u16();
                        int_headers.reserve(kv_size as usize);
                        for _ in 0..kv_size {
                            remaining_header_size -= 4;
                            let key = src.get_u16();
                            let value_len = src.get_u16();
                            remaining_header_size -= value_len as usize;
                            let mut value = vec![0u8; value_len as usize];
                            src.copy_to_slice(&mut value);
                            let key = IntMetaKey::try_from(key).map_err(|e| {
                                new_protocol_error(
                                    ProtocolErrorKind::InvalidData,
                                    format!("invalid int meta key {}: {}", key, e),
                                )
                            })?;

                            int_headers.insert(
                                key,
                                from_utf8(&value)
                                    .map_err(|e| {
                                        new_protocol_error(
                                            ProtocolErrorKind::InvalidData,
                                            format!("invalid int meta value {:?}: {}", value, e),
                                        )
                                    })?
                                    .to_string(),
                            );
                        }
                    }
                    info::ACL_TOKEN_KEY_VALUE => {
                        remaining_header_size -= 2;
                        let token_len = src.get_u16();
                        // just ignore token
                        remaining_header_size -= token_len as usize;
                        let mut token = vec![0u8; token_len as usize];
                        src.copy_to_slice(&mut token);
                    }
                    _ => {
                        let msg = format!("unexpected info id in ttheader: {}", info_id);
                        warn!("[VOLO] {}", msg);
                        return Err(crate::Error::Pilota(new_protocol_error(ProtocolErrorKind::Unknown, msg)));
                    }
                }
            }

            let role = cx.rpc_info().role();
            match role {
                Role::Client => {
                    if let Some(ad) = headers.remove(HEADER_TRANS_REMOTE_ADDR) {
                        if let Some(_host) = ad.split(':').next() {
                            // TODO: get_idc_from_ip and set tag
                        }
                        let maybe_addr = ad.parse::<SocketAddr>();
                        if let (Some(callee), Ok(addr)) =
                            (cx.rpc_info_mut().callee.as_mut(), maybe_addr)
                        {
                            callee.set_address(volo::net::Address::from(addr));
                        }
                    }
                    if let Some(crrst) = headers.remove(HEADER_CONNECTION_READY_TO_RESET) {
                        if !crrst.is_empty() {
                            cx.set_conn_reset_by_ttheader(true);
                        }
                    }

                    // Search for backward metainfo.
                    // We are not supposed to use headers, so we can use into_iter to avoid clone.
                    for (k, v) in headers.into_iter() {
                        if k.starts_with(metainfo::RPC_PREFIX_BACKWARD) {
                            metainfo.strip_rpc_prefix_and_set_backward_downstream(k, v);
                        }
                    }
                }
                Role::Server => {
                    // Caller
                    let from_service = int_headers
                        .remove_entry(&IntMetaKey::FromService)
                        .map(|(_, v)| v);

                    if let Some(from_service) = from_service {
                        let mut caller = Endpoint::new(Arc::<str>::from(from_service).into());
                        if let Some(ad) = headers.remove(HEADER_TRANS_REMOTE_ADDR) {
                            let addr = ad.parse::<SocketAddr>();
                            if let Ok(addr) = addr {
                                caller.set_address(volo::net::Address::from(addr));
                            }
                        }

                        if caller.address.is_none() {
                            if let Some(v) = cx
                                .rpc_info_mut()
                                .caller
                                .as_mut()
                                .and_then(|x| x.address.take())
                            {
                                caller.set_address(v);
                            }
                        }
                        cx.rpc_info_mut().caller = Some(caller);
                    }

                    // Callee
                    let to_service = int_headers
                        .remove_entry(&IntMetaKey::ToService)
                        .map(|(_, v)| v);

                    if let Some(to_service) = to_service {
                        let callee = Endpoint::new(Arc::<str>::from(to_service).into());

                        cx.rpc_info_mut().callee = Some(callee);
                    }

                    // Config
                    let mut config = Config::new();
                    if let Some(Ok(rpc_timeout)) = int_headers
                        .get(&IntMetaKey::RPCTimeout)
                        .map(|x| x.parse().map(Duration::from_millis))
                    {
                        config.set_rpc_timeout(Some(rpc_timeout));
                    }

                    cx.rpc_info_mut().config = Some(config);

                    // Search for forward metainfo.
                    // We are not supposed to use headers, so we can use into_iter to avoid clone.
                    for (k, v) in headers.into_iter() {
                        if k.starts_with(metainfo::RPC_PREFIX_PERSISTENT) {
                            metainfo.strip_rpc_prefix_and_set_persistent(k, v);
                        } else if k.starts_with(metainfo::RPC_PREFIX_TRANSIENT) {
                            metainfo.strip_rpc_prefix_and_set_upstream(k, v);
                        }
                    }
                }
            }
            Ok(())
        })
}
