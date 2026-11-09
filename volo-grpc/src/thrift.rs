//! Helpers for the explicit Thrift-over-gRPC transport.

mod send;
use pilota::thrift::Message;
pub use send::{
    ChannelConfig, IntoStreamingRequest, SendStream, Sender, channel, channel_with_config,
};

use crate::{Status, codec::thrift::ThriftRecvStream};

/// Kitex business status carried by final gRPC metadata, independently of a
/// unary method's typed declared exceptions and any BaseResp message field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BizStatus {
    pub code: i32,
    pub message: String,
    pub extra: std::collections::BTreeMap<String, String>,
}

impl BizStatus {
    /// Kitex represents a business failure with nonzero grpc-status (Internal
    /// by default), biz-status, biz-extra and percent-encoded grpc-message.
    pub fn into_status(self) -> Status {
        if self.code == 0 {
            return Status::invalid_argument("business error code must be nonzero");
        }
        let mut status = Status::internal(self.message);
        status.metadata_mut().insert(
            "biz-status",
            self.code.to_string().parse().expect("integer is ASCII"),
        );
        let json = serde_json::to_vec(&self.extra).expect("string map serializes to JSON");
        status.metadata_mut().insert(
            "biz-extra",
            crate::metadata::MetadataValue::try_from_bytes(&json)
                .expect("JSON escapes control characters"),
        );
        status
    }

    pub fn from_status(status: &Status) -> Result<Option<Self>, Status> {
        let Some(code) = status.metadata().get("biz-status") else {
            return Ok(None);
        };
        let code = std::str::from_utf8(code.as_bytes())
            .ok()
            .and_then(|value| value.parse::<i32>().ok());
        // Match Kitex: absent, invalid and zero business codes are not errors.
        let Some(code) = code.filter(|code| *code != 0) else {
            return Ok(None);
        };
        let extra = match status.metadata().get("biz-extra") {
            Some(value) => serde_json::from_slice::<
                Option<std::collections::BTreeMap<String, String>>,
            >(value.as_bytes())
            .map_err(|_| Status::internal("invalid biz-extra JSON"))?
            .unwrap_or_default(),
            None => Default::default(),
        };
        Ok(Some(Self {
            code,
            message: status.message().to_owned(),
            extra,
        }))
    }
}

/// Receive exactly one request, validating END_STREAM before invoking unary code.
#[doc(hidden)]
pub async fn read_unary_request<T: Message>(
    messages: &mut ThriftRecvStream<T>,
) -> Result<T, Status> {
    let value = messages
        .message()
        .await?
        .ok_or_else(|| Status::invalid_argument("Missing unary request message"))?;
    if messages.message().await?.is_some() {
        return Err(Status::invalid_argument("Multiple unary request messages"));
    }
    Ok(value)
}

/// Receive one response and its successful final status before returning its value.
#[doc(hidden)]
pub async fn read_unary_response<T: Message>(
    messages: &mut ThriftRecvStream<T>,
) -> Result<T, Status> {
    let value = messages
        .message()
        .await?
        .ok_or_else(|| Status::internal("Missing unary response message"))?;
    if messages.message().await?.is_some() {
        return Err(Status::internal("Multiple unary response messages"));
    }
    Ok(value)
}

#[cfg(test)]
mod biz_tests {
    use super::*;

    #[test]
    fn json_header_values_preserve_unicode_and_escape_quotes_backslashes_and_controls() {
        let biz = BizStatus {
            code: -7,
            message: "失败：取消%\n".into(),
            extra: [("中文\\\"".into(), "换行\n\t\0".into())].into(),
        };
        let status = biz.clone().into_status();
        assert!(status.metadata().get("biz-message").is_none());
        let wire = status.to_header_map().unwrap();
        let decoded = Status::from_header_map(&wire).unwrap();
        assert_eq!(BizStatus::from_status(&decoded).unwrap(), Some(biz));
    }
}
