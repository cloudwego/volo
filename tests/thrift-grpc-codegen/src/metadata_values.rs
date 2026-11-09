//! Opaque and repeated gRPC metadata over the real generated Thrift service.
use futures::{StreamExt, stream};
use volo_grpc::{
    Request, Response, Status,
    codec::thrift::ThriftRecvStream,
    metadata::{Ascii, MetadataKey, MetadataMap, MetadataValue},
    streaming::StreamContext,
    thrift::SendStream,
};

use crate::volo_gen::grpc_thrift::{Empty, PureStreaming, PureStreamingClient};

const BINARY_VALUES: [&[u8]; 2] = [b"\x00\xff\x80,\x01", b"second\x00value"];

fn fields() -> MetadataMap {
    let mut fields = MetadataMap::new();
    // API key normalization must preserve the value, while H2 wire names stay
    // lowercase. Binary values are decoded bytes rather than UTF-8 strings.
    let key = MetadataKey::<Ascii>::from_bytes(b"X-Interop-Ascii").unwrap();
    fields.insert(key, "original-value".parse().unwrap());
    for value in ["first", "second"] {
        fields.append("x-interop-multi", value.parse().unwrap());
    }
    for value in BINARY_VALUES {
        fields.append_bin("x-interop-opaque-bin", MetadataValue::from_bytes(value));
    }
    fields
}

fn check(fields: &MetadataMap, phase: Option<&str>) -> Result<(), Status> {
    if fields.get("x-interop-ascii").map(|value| value.as_bytes()) != Some(b"original-value")
        || fields
            .get_all("x-interop-multi")
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>()
            != [b"first".as_slice(), b"second"]
        || fields
            .get_all_bin("x-interop-opaque-bin")
            .iter()
            .map(|value| value.to_bytes().map(|value| value.to_vec()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Status::internal("invalid binary metadata encoding"))?
            != BINARY_VALUES
        || phase.is_some_and(|phase| {
            fields.get("x-interop-phase").map(|value| value.as_bytes()) != Some(phase.as_bytes())
        })
    {
        return Err(Status::internal("opaque or repeated metadata differs"));
    }
    Ok(())
}

pub struct MetadataValuesHandler;

impl PureStreaming for MetadataValuesHandler {
    async fn bidi(
        &self,
        request: Request<ThriftRecvStream<Empty>>,
    ) -> Result<Response<SendStream<Empty>>, Status> {
        if request.metadata().contains_key("x-interop-ascii") {
            check(request.metadata(), None)?;
            let cx = request
                .extensions()
                .get::<StreamContext>()
                .ok_or_else(|| Status::internal("missing stream context"))?;
            let mut headers = fields();
            headers.insert("x-interop-phase", "header".parse().unwrap());
            cx.response_headers(headers)?;
            let mut trailers = fields();
            trailers.insert("x-interop-phase", "trailer".parse().unwrap());
            cx.response_trailers(trailers)?;
        }
        Ok(Response::new(SendStream::from_stream(request.into_inner())))
    }
}

pub async fn verify(client: &PureStreamingClient) -> Result<(), Status> {
    let mut request = Request::new(stream::iter([Empty {}]));
    *request.metadata_mut() = fields();
    let response = client.bidi(request).await?;
    check(response.metadata(), Some("header"))?;
    let mut response = response.into_inner();
    if response.next().await.transpose()? != Some(Empty {})
        || response.next().await.transpose()?.is_some()
    {
        return Err(Status::internal(
            "metadata probe message cardinality differs",
        ));
    }
    check(
        &response
            .trailers()
            .await?
            .ok_or_else(|| Status::internal("missing metadata trailers"))?,
        Some("trailer"),
    )
}
