#![allow(dead_code)]

//! TTheader is a transport protocol designed by CloudWeGo.
//!
//! For more information, please visit https://www.cloudwego.io/docs/kitex/reference/transport_protocol_ttheader/

use std::{
    collections::HashMap, convert::TryFrom, default::Default, net::SocketAddr, str::from_utf8,
    sync::Arc, time::Duration,
};

use bytes::{Buf, BufMut, BytesMut};
use metainfo::{Backward, Forward};
use num_enum::TryFromPrimitive;
use tracing::{trace, warn};
use volo::context::{Endpoint, Role};

use crate::{
    context::{Config, ThriftContext},
    error::{new_protocol_error, ProtocolErrorKind},
    tags::TransportType,
};

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

#[derive(TryFromPrimitive)]
#[repr(u8)]
pub enum ProtocolId {
    Binary = 0,
    Compact = 2,
    CompactV2 = 3,
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
    // framed / unframed
    TransportType = 1,
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

// The max message size is limited to 16M. TODO: allow config
const MAX_TT_HEADER_SIZE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct DefaultTTHeaderCodec;

impl TTHeaderEncoder for DefaultTTHeaderCodec {
    fn encode<Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        dst: &mut BytesMut,
        size: usize,
    ) -> Result<usize, crate::Error> {
        let thrift_cx = cx;
        Ok(metainfo::METAINFO.with(|metainfo| {
            let metainfo = metainfo.borrow_mut();
            let zero_index = dst.len();
            // Alloc 4-byte space as length
            dst.reserve(4);
            unsafe {
                dst.advance_mut(4);
            }

            // tt header magic
            dst.put_u16(super::magic::TT_HEADER);
            // flags
            dst.put_u16(0);
            let seq_id = thrift_cx.seq_id();
            dst.put_u32(seq_id as u32); // TODO: thrift seq_id is i32, tt header is u32?

            // Alloc 2-byte space as header length
            dst.reserve(2);
            unsafe {
                dst.advance_mut(2);
            }

            // protocol_id
            dst.put_u8(0); // TODO: item.protocol_id as u8(0=Binary; 2=Compact)
            dst.put_u8(0); // TODO: item.transform_ids_num

            // TODO: item.transform_ids
            // if let Some(ids) = &item.transform_ids {
            //     dst.put_slice(ids);
            // }

            let role = thrift_cx.rpc_info().role();

            // Write string KV start.

            let has_string_kv = match role {
                Role::Client => {
                    metainfo.get_all_persistents().is_some()
                        || metainfo.get_all_transients().is_some()
                }
                Role::Server => {
                    metainfo.get_all_backward_transients().is_some()
                        || thrift_cx.encode_conn_reset().unwrap_or(false)
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
                        if thrift_cx.encode_conn_reset().unwrap_or(false) {
                            dst.put_u16(5);
                            dst.put_slice("crrst".as_bytes());
                            dst.put_u16(1);
                            dst.put_slice("1".as_bytes());
                            string_kv_len += 1;
                        }
                    }
                }

                let mut buf = &mut dst[string_kv_index..string_kv_index + 2];
                buf.put_u16(string_kv_len as u16);
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
                    let msg_type: u8 = thrift_cx.msg_type().into();
                    dst.put_u16(IntMetaKey::MsgType as u16);
                    dst.put_u16(1);
                    dst.put_slice(&[msg_type]);
                    int_kv_len += 1;
                }

                Role::Client => {
                    // TransportType
                    if let Some(transport_type) = thrift_cx.extensions().get::<TransportType>() {
                        dst.put_u16(IntMetaKey::TransportType as u16);
                        dst.put_u16(transport_type.len() as u16);
                        dst.put_slice(transport_type.as_bytes());
                        int_kv_len += 1;
                    };

                    // WithHeader
                    dst.put_u16(IntMetaKey::WithHeader as u16);
                    dst.put_u16(1);
                    dst.put_slice("3".as_bytes());
                    int_kv_len += 1;

                    // Config
                    if let Some(config) = thrift_cx.rpc_info().config.as_ref() {
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
                    if let Some(caller) = thrift_cx.rpc_info().caller.as_ref() {
                        let svc = caller.service_name();
                        dst.put_u16(IntMetaKey::FromService as u16);
                        dst.put_u16(svc.len() as u16);
                        dst.put_slice(svc.as_bytes());
                        int_kv_len += 1;
                    }

                    // Callee
                    if let Some(callee) = thrift_cx.rpc_info().callee.as_ref() {
                        let method = thrift_cx.rpc_info().method.as_ref().unwrap();
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
            buf.put_u16(((header_size - 14) / 4).try_into().unwrap());
            trace!(
                "[VOLO] encode ttheader write header size: {}",
                (header_size - 14) / 4
            );

            let size = header_size + size;

            // fill length
            let mut buf = &mut dst[zero_index..zero_index + 4];
            buf.put_u32((size - 4).try_into().unwrap());
            trace!("[VOLO] encode ttheader write length size: {}", size - 4);
            header_size
        }))
    }
}

impl TTHeaderDecoder for DefaultTTHeaderCodec {
    fn decode<Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        src: &mut BytesMut,
    ) -> Result<(), crate::Error> {
        let thrift_cx = cx;
        metainfo::METAINFO.with(|metainfo| {
            let metainfo = &mut *metainfo.borrow_mut();
            let _magic = src.get_u16();
            let _flags = src.get_u16();
            let _sequence_id = src.get_u32();
            let header_size = src.get_u16();
            let _protocol_id = src.get_u8();
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
                                        )
                                    })?
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
                        return Err(new_protocol_error(ProtocolErrorKind::Unknown, msg));
                    }
                }
            }

            let role = thrift_cx.rpc_info().role();
            match role {
                Role::Client => {
                    if let Some(ad) = headers.remove(HEADER_TRANS_REMOTE_ADDR) {
                        if let Some(_host) = ad.split(':').next() {
                            // TODO: get_idc_from_ip and set tag
                        }
                        let maybe_addr = ad.parse::<SocketAddr>();
                        if let (Some(callee), Ok(addr)) =
                            (thrift_cx.rpc_info_mut().callee.as_mut(), maybe_addr)
                        {
                            callee.set_address(volo::net::Address::from(addr));
                        }
                    }
                    if let Some(crrst) = headers.remove(HEADER_CONNECTION_READY_TO_RESET) {
                        if !crrst.is_empty() {
                            thrift_cx.set_conn_reset_by_ttheader(true);
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
                            if let Some(v) = thrift_cx
                                .rpc_info_mut()
                                .caller
                                .as_mut()
                                .and_then(|x| x.address.take())
                            {
                                caller.set_address(v);
                            }
                        }
                        thrift_cx.rpc_info_mut().caller = Some(caller);
                    }

                    // Callee
                    let to_service = int_headers
                        .remove_entry(&IntMetaKey::ToService)
                        .map(|(_, v)| v);

                    if let Some(to_service) = to_service {
                        let callee = Endpoint::new(Arc::<str>::from(to_service).into());

                        thrift_cx.rpc_info_mut().callee = Some(callee);
                    }

                    // Config
                    let mut config = Config::new();
                    if let Some(Ok(rpc_timeout)) = int_headers
                        .get(&IntMetaKey::RPCTimeout)
                        .map(|x| x.parse::<u64>().map(Duration::from_millis))
                    {
                        config.set_rpc_timeout(Some(rpc_timeout));
                    }

                    thrift_cx.rpc_info_mut().config = Some(config);

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
}

pub trait TTHeaderEncoder: Copy + Send + Sync + 'static {
    fn encode<Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        dst: &mut BytesMut,
        size: usize,
    ) -> Result<usize, crate::Error>;
}

pub trait TTHeaderDecoder: Copy + Send + Sync + 'static {
    fn decode<Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        src: &mut BytesMut,
    ) -> Result<(), crate::Error>;
}
