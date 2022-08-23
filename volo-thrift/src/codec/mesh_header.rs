#![allow(dead_code)]

use std::{collections::HashMap, net::SocketAddr, str::from_utf8};

use bytes::{Buf, BytesMut};

use crate::{
    codec::tt_header::{HEADER_CONNECTION_READY_TO_RESET, HEADER_TRANS_REMOTE_ADDR},
    context::ThriftContext,
    error::{new_protocol_error, ProtocolErrorKind},
};

/// +-------------2Byte-------------|-------------2Byte--------------+
/// +----------------------------------------------------------------+
/// |       HEADER MAGIC            |      HEADER SIZE               |
/// +----------------------------------------------------------------+
/// |       HEADER MAP SIZE         |    HEADER MAP...               |
/// +----------------------------------------------------------------+
/// |                                                                |
/// |                            PAYLOAD                             |
/// |                                                                |
/// +----------------------------------------------------------------+

pub fn decode(src: &mut BytesMut, cx: &mut impl ThriftContext) -> Result<(), crate::Error> {
    let kv_size = src.get_u16();
    let mut headers = HashMap::with_capacity(kv_size as usize);
    for _ in 0..kv_size {
        let key_len = src.get_u16();
        let mut key = vec![0u8; key_len as usize];
        src.copy_to_slice(&mut key);
        let value_len = src.get_u16();
        let mut value = vec![0u8; value_len as usize];
        src.copy_to_slice(&mut value);
        headers.insert(
            from_utf8(&key)
                .map_err(|e| {
                    new_protocol_error(
                        ProtocolErrorKind::InvalidData,
                        format!("invalid header key which is not utf-8 {:?}: {}", key, e),
                    )
                })?
                .to_string(),
            from_utf8(&value)
                .map_err(|e| {
                    new_protocol_error(
                        ProtocolErrorKind::InvalidData,
                        format!("invalid header value which is not utf-8 {:?}: {}", key, e),
                    )
                })?
                .to_string(),
        );
    }

    if let Some(ad) = headers.remove(HEADER_TRANS_REMOTE_ADDR) {
        let maybe_addr = ad.parse::<SocketAddr>();
        if let (Some(callee), Ok(addr)) = (cx.rpc_info_mut().callee.as_mut(), maybe_addr) {
            callee.set_address(volo::net::Address::from(addr));
        }
    }
    if let Some(crrst) = headers.remove(HEADER_CONNECTION_READY_TO_RESET) {
        if !crrst.is_empty() {
            // mark the connection not reusable
            cx.set_conn_reset_by_ttheader(true);
        }
    }
    Ok(())
}
