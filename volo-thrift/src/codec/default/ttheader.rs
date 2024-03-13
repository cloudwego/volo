//! TTheader is a transport protocol designed by CloudWeGo.
//!
//! For more information, please visit https://www.cloudwego.io/docs/kitex/reference/transport_protocol_ttheader/

#![allow(clippy::mutable_key_type)]

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use linkedbytes::LinkedBytes;
use metainfo::{Backward, Forward};
use num_enum::TryFromPrimitive;
use pilota::thrift::{
    new_protocol_exception, ProtocolException, ProtocolExceptionKind, ThriftException,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt};
use tracing::{trace, warn};
use volo::{context::Role, util::buf_reader::BufReader, FastStr};

use super::MakeZeroCopyCodec;
use crate::{
    codec::default::{ZeroCopyDecoder, ZeroCopyEncoder},
    context::ThriftContext,
    BizError, EntryMessage, ThriftMessage,
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
pub struct HasTTHeader;

struct BizErrorExtra(FastStr);

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

impl<D> ZeroCopyDecoder for TTHeaderDecoder<D>
where
    D: ZeroCopyDecoder,
{
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        bytes: &mut Bytes,
    ) -> Result<Option<ThriftMessage<Msg>>, ThriftException> {
        if bytes.len() < HEADER_DETECT_LENGTH {
            // not enough bytes to detect, must not be TTHeader, so just forward to inner
            return self.inner.decode(cx, bytes);
        }

        if is_ttheader(&bytes[..HEADER_DETECT_LENGTH]) {
            let _size = bytes.get_u32() as usize;
            // decode ttheader
            decode(cx, bytes)?;
            // set has ttheader flag
            cx.extensions_mut().insert(HasTTHeader);
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
    ) -> Result<Option<ThriftMessage<Msg>>, ThriftException> {
        // check if is ttheader
        if let Ok(buf) = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await {
            if is_ttheader(buf) {
                // read all the data out, and call inner decode instead of decode_async
                let size = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
                cx.stats_mut().set_read_size(size + 4);

                reader.consume(4);
                let mut buffer = BytesMut::with_capacity(size);
                unsafe {
                    buffer.set_len(size);
                }
                reader.read_exact(&mut buffer[..size]).await?;

                cx.stats_mut().record_read_end_at();

                let mut buffer = buffer.freeze();

                // decode ttheader
                decode(cx, &mut buffer)?;
                // set has ttheader flag
                cx.extensions_mut().insert(HasTTHeader);
                // decode inner
                self.inner.decode(cx, &mut buffer)
            } else {
                // no TTHeader, just forward to inner decoder
                self.inner.decode_async(cx, reader).await
            }
        } else {
            self.inner.decode_async(cx, reader).await
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
    ) -> Result<(), ThriftException> {
        let dst = linked_bytes.bytes_mut();
        // only encode ttheader if role is client or server has detected ttheader in decode
        if cx.rpc_info().role() == Role::Client || cx.extensions().contains::<HasTTHeader>() {
            // encode ttheader first
            encode(cx, dst, self.inner_size)?;
        }
        self.inner.encode(cx, linked_bytes, msg)
    }

    fn size<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: &ThriftMessage<Msg>,
    ) -> Result<(usize, usize), ThriftException> {
        let (real_size, malloc_size) = self.inner.size(cx, msg)?;
        self.inner_size = real_size;
        // only calc ttheader size if role is client or server has detected ttheader in decode
        if cx.rpc_info().role() == Role::Client || cx.extensions().contains::<HasTTHeader>() {
            let size = encode_size(cx)?;
            Ok((real_size + size, malloc_size + size))
        } else {
            Ok((real_size, malloc_size))
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
pub(crate) const TT_HEADER_BIZ_STATUS_KEY: &str = "biz-status";
pub(crate) const TT_HEADER_BIZ_MESSAGE_KEY: &str = "biz-message";
pub(crate) const TT_HEADER_BIZ_EXTRA_KEY: &str = "biz-extra";

#[derive(TryFromPrimitive, Clone, Copy, Default)]
#[repr(u8)]
pub enum ProtocolId {
    #[default]
    Binary = 0,
    Compact = 2,   // Apache Thrift compact protocol
    CompactV2 = 3, // fbthrift compact protocol
    Protobuf = 4,
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
) -> Result<(), ThriftException> {
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
                    || cx.stats().biz_error().is_some()
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

                    if let Some(biz_error) = cx.stats().biz_error() {
                        let mut ibuf = itoa::Buffer::new();
                        let status_code = ibuf.format(biz_error.status_code);
                        dst.put_u16(TT_HEADER_BIZ_STATUS_KEY.as_bytes().len() as u16);
                        dst.put_slice(TT_HEADER_BIZ_STATUS_KEY.as_bytes());
                        dst.put_u16(status_code.len() as u16);
                        dst.put_slice(status_code.as_bytes());
                        string_kv_len += 1;

                        dst.put_u16(TT_HEADER_BIZ_MESSAGE_KEY.as_bytes().len() as u16);
                        dst.put_slice(TT_HEADER_BIZ_MESSAGE_KEY.as_bytes());
                        dst.put_u16(biz_error.status_message.len() as u16);
                        dst.put_slice(biz_error.status_message.as_bytes());
                        string_kv_len += 1;

                        if let Some(extra) = &biz_error.extra {
                            let extra_str = if let Some(extra) =
                                cx.extensions().get::<BizErrorExtra>().map(|e| e.0.clone())
                            {
                                extra
                            } else {
                                sonic_rs::to_string(&extra)
                                    .expect("encode biz error extra")
                                    .into()
                            };
                            dst.put_u16(TT_HEADER_BIZ_EXTRA_KEY.as_bytes().len() as u16);
                            dst.put_slice(TT_HEADER_BIZ_EXTRA_KEY.as_bytes());
                            dst.put_u16(extra_str.as_bytes().len() as u16);
                            dst.put_slice(extra_str.as_bytes());
                            string_kv_len += 1;
                        }
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
                let config = cx.rpc_info().config();
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

                // Caller
                let caller = cx.rpc_info().caller();
                let svc = caller.service_name();
                dst.put_u16(IntMetaKey::FromService as u16);
                dst.put_u16(svc.len() as u16);
                dst.put_slice(svc.as_bytes());
                int_kv_len += 1;

                // Callee
                let callee = cx.rpc_info().callee();
                let method = cx.rpc_info().method();
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

        if let Ok(written_header_size) = written_header_size.try_into() {
            buf.put_u16(written_header_size);
            trace!(
                "[VOLO] encode ttheader write header size: {}",
                written_header_size
            );
        } else {
            return Err(ProtocolException::new(
                ProtocolExceptionKind::SizeLimit,
                format!("ttheader header size {written_header_size} overflows u16"),
            ));
        }

        let size = header_size + size;

        // fill length
        let mut buf = &mut dst[zero_index..zero_index + 4];
        if let Ok(ttheader_size) = (size - 4).try_into() {
            buf.put_u32(ttheader_size);
            trace!(
                "[VOLO] encode ttheader write length size: {}",
                ttheader_size
            );
        } else {
            return Err(ProtocolException::new(
                ProtocolExceptionKind::SizeLimit,
                format!("ttheader size {size} overflows u32"),
            ));
        }
        Ok(())
    })?;
    Ok(())
}

// this must be with sync to the encode impl
pub(crate) fn encode_size<Cx: ThriftContext>(cx: &mut Cx) -> Result<usize, ThriftException> {
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
                    if let Some(biz_error) = thrift_cx.stats().biz_error() {
                        len += 2;
                        len += TT_HEADER_BIZ_STATUS_KEY.as_bytes().len();
                        len += 2;
                        len += itoa::Buffer::new()
                            .format(biz_error.status_code)
                            .as_bytes()
                            .len();

                        len += 2;
                        len += TT_HEADER_BIZ_MESSAGE_KEY.as_bytes().len();
                        len += 2;
                        len += biz_error.status_message.as_bytes().len();

                        if let Some(extra) = &biz_error.extra {
                            len += 2;
                            len += TT_HEADER_BIZ_EXTRA_KEY.as_bytes().len();
                            len += 2;
                            let extra =
                                sonic_rs::to_string(&extra).expect("encode biz error extra");
                            len += extra.as_bytes().len();
                            thrift_cx
                                .extensions_mut()
                                .insert(BizErrorExtra(extra.into()));
                        }
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
                let config = thrift_cx.rpc_info().config();
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

                // Caller
                let caller = thrift_cx.rpc_info().caller();
                let svc = caller.service_name();
                len += 2;
                len += 2;
                len += svc.as_bytes().len();

                // Callee
                let callee = thrift_cx.rpc_info().callee();
                let method = thrift_cx.rpc_info().method();
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
    src: &mut Bytes,
) -> Result<(), ThriftException> {
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
                    pilota::thrift::new_protocol_exception(
                        pilota::thrift::ProtocolExceptionKind::BadVersion,
                        format!("unknown protocol id: {protocol_id} in ttheader")
                    )
                );
            }

            let transform_ids_num = src.get_u8();
            let mut _transform_ids = None;
            if transform_ids_num > 0 {
                let _transform_ids_inner = src.split_to(transform_ids_num as usize);
                _transform_ids = Some(_transform_ids_inner);
            }

            #[allow(clippy::mutable_key_type)]
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
                            let key = src.split_to(key_len as usize);

                            remaining_header_size -= 2;
                            let value_len = src.get_u16();
                            remaining_header_size -= value_len as usize;
                            let value = src.split_to(value_len as usize);

                            headers.insert(
                                unsafe { FastStr::from_bytes_unchecked(key) },
                                unsafe { FastStr::from_bytes_unchecked(value) }
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
                            let value_len = src.get_u16() as usize;
                            remaining_header_size -= value_len;
                            let value = src.split_to(value_len);
                            let key = match IntMetaKey::try_from(key) {
                                Ok(k) => k,
                                Err(e) => {
                                    tracing::debug!("[VOLO] unknown int header key: {}, value: {:?}, error: {}", key, value, e);
                                    continue;
                                },
                            };

                            int_headers.insert(
                                key,
                                unsafe { FastStr::from_bytes_unchecked(value) }
                            );
                        }
                    }

                    info::ACL_TOKEN_KEY_VALUE => {
                        remaining_header_size -= 2;
                        let token_len = src.get_u16();
                        // just ignore token
                        remaining_header_size -= token_len as usize;
                        let _token = src.split_to(token_len as usize);
                    }
                    _ => {
                        let msg = format!("unexpected info id in ttheader: {info_id}");
                        warn!("[VOLO] {}", msg);
                        return Err(new_protocol_exception( pilota::thrift::ProtocolExceptionKind::InvalidData, msg));
                    }
                }
            }

            let role = cx.rpc_info().role();
            match role {
                Role::Client => {
                    if let Some(ad) = headers.remove(HEADER_TRANS_REMOTE_ADDR) {
                        // if let Some(_host) = ad.split(':').next() {
                            // TODO: get_idc_from_ip and set tag
                        // }
                        let maybe_addr = ad.parse::<SocketAddr>();
                        if let Ok(addr) = maybe_addr {
                            cx.rpc_info_mut().callee_mut().set_address(volo::net::Address::from(addr));
                        }
                    }
                    if let Some(crrst) = headers.remove(HEADER_CONNECTION_READY_TO_RESET) {
                        if !crrst.is_empty() {
                            cx.set_conn_reset_by_ttheader(true);
                        }
                    }

                    set_biz_error_header(cx, &mut headers);

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
                        let caller = cx.rpc_info_mut().caller_mut();
                        caller.set_service_name(from_service);
                        if let Some(ad) = headers.remove(HEADER_TRANS_REMOTE_ADDR) {
                            let addr = ad.parse::<SocketAddr>();
                            if let Ok(addr) = addr {
                                caller.set_address(volo::net::Address::from(addr));
                            }
                        }
                    }

                    // Callee
                    let to_service = int_headers
                        .remove_entry(&IntMetaKey::ToService)
                        .map(|(_, v)| v);

                    if let Some(to_service) = to_service {
                        cx.rpc_info_mut().callee_mut().set_service_name(to_service);
                    }

                    // Config
                    if let Some(Ok(rpc_timeout)) = int_headers
                        .get(&IntMetaKey::RPCTimeout)
                        .map(|x| x.parse().map(Duration::from_millis))
                    {
                        cx.rpc_info_mut().config_mut().set_rpc_timeout(Some(rpc_timeout));
                    }

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

fn set_biz_error_header<Cx: ThriftContext>(
    thrift_cx: &mut Cx,
    headers: &mut HashMap<FastStr, FastStr>,
) {
    let biz_error = BizError {
        status_code: if let Some(biz_status) = headers.remove(TT_HEADER_BIZ_STATUS_KEY) {
            if let Ok(status_code) = biz_status.parse() {
                if status_code == 0 {
                    // align with kitex, 0 means no biz error
                    return;
                }
                status_code
            } else {
                warn!(
                    "[VOLO] \"biz-status\" key found in ttheader, but value is not a valid i32 \
                     string: {}, rpcinfo: {:?}",
                    biz_status,
                    thrift_cx.rpc_info()
                );
                return;
            }
        } else {
            return;
        },
        status_message: headers
            .remove(TT_HEADER_BIZ_MESSAGE_KEY)
            .unwrap_or_default(),
        extra: headers
            .remove(TT_HEADER_BIZ_EXTRA_KEY)
            .and_then(|biz_extra| {
                sonic_rs::from_str(&biz_extra)
                    .map_err(|e| {
                        warn!(
                            "[VOLO] \"biz-extra\" key found in ttheader, but value is not a valid \
                             json string: {}, rpcinfo: {:?}, error: {}",
                            biz_extra,
                            thrift_cx.rpc_info(),
                            e
                        )
                    })
                    .unwrap_or_default()
            }),
    };

    thrift_cx.stats_mut().set_biz_error(biz_error);
}
