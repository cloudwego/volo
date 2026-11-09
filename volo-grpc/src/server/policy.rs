use crate::{Status, body::BoxBody, context::ServerContext, message::PayloadKind};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ResponsePolicy {
    pub payload: PayloadKind,
    pub http_status: Option<http::StatusCode>,
}

#[derive(Clone, Debug)]
pub(super) struct HttpRequestInfo {
    pub method: http::Method,
    pub version: http::Version,
}

pub(super) fn validate_request_info(
    info: Option<&HttpRequestInfo>,
    payload: PayloadKind,
    cx: &mut ServerContext,
) -> Result<(), Status> {
    if payload != PayloadKind::Thrift {
        return Ok(());
    }
    if let Some(info) = info {
        let status = if info.method != http::Method::POST {
            Some(http::StatusCode::METHOD_NOT_ALLOWED)
        } else if info.version != http::Version::HTTP_2 {
            Some(http::StatusCode::HTTP_VERSION_NOT_SUPPORTED)
        } else {
            None
        };
        if let Some(status) = status {
            cx.extensions.insert(ResponsePolicy {
                payload,
                http_status: Some(status),
            });
            return Err(Status::unimplemented("Thrift gRPC requires HTTP/2 POST"));
        }
    }
    Ok(())
}

impl ResponsePolicy {
    pub fn apply(self, response: &mut http::Response<BoxBody>) {
        response.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static(self.payload.content_type()),
        );
        if let Some(status) = self.http_status {
            *response.status_mut() = status;
        }
    }
}

pub(super) fn error_response(status: Status, cx: &ServerContext) -> http::Response<BoxBody> {
    let mut response = status.to_http();
    if let Some(stream) = cx.extensions.get::<crate::streaming::StreamContext>() {
        let mut metadata = stream.response_metadata(false);
        metadata.merge(stream.response_metadata(true));
        let mut headers = metadata.into_headers();
        headers.extend(std::mem::take(response.headers_mut()));
        *response.headers_mut() = headers;
    }
    cx.extensions
        .get::<ResponsePolicy>()
        .copied()
        .unwrap_or_default()
        .apply(&mut response);
    response
}

pub(super) fn validate_content_type(
    headers: &http::HeaderMap,
    payload: PayloadKind,
    cx: &mut ServerContext,
) -> Result<(), Status> {
    if payload != PayloadKind::Thrift {
        return Ok(());
    }
    let values = headers.get_all(http::header::CONTENT_TYPE);
    let mut values = values.iter();
    let value = values.next();
    let duplicate = values.next().is_some();
    let media = value.map(|value| {
        value
            .as_bytes()
            .split(|byte| *byte == b';')
            .next()
            .unwrap_or_default()
            .trim_ascii()
    });
    if !duplicate && media == Some(b"application/grpc+thrift") {
        return Ok(());
    }
    let grpc = !duplicate
        && media.is_some_and(|media| {
            media == b"application/grpc" || media.starts_with(b"application/grpc+")
        });
    if !grpc {
        cx.extensions.insert(ResponsePolicy {
            payload,
            http_status: Some(http::StatusCode::UNSUPPORTED_MEDIA_TYPE),
        });
    }
    Err(Status::unimplemented(
        "method requires application/grpc+thrift",
    ))
}
