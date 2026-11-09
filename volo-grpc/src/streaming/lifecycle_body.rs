//! Body ownership outlives the motore call and receives Hyper's reset/EOF Drop.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::{Body, Frame};
use pin_project::pin_project;
use tokio_util::sync::WaitForCancellationFutureOwned;

use super::{CallGuard, StreamContext};
use crate::{
    Code, Status,
    body::{BoxBody, boxed, empty_body},
    metadata::MetadataMap,
};

pub(crate) fn request(body: BoxBody, cx: StreamContext) -> BoxBody {
    boxed(LifecycleBody::new(body, cx, None))
}

pub(crate) fn response(body: BoxBody, guard: CallGuard) -> BoxBody {
    boxed(LifecycleBody::new(body, guard.context.clone(), Some(guard)))
}

#[pin_project]
struct LifecycleBody {
    inner: BoxBody,
    context: StreamContext,
    #[pin]
    cancelled: WaitForCancellationFutureOwned,
    guard: Option<CallGuard>,
    done: bool,
}

impl LifecycleBody {
    fn new(inner: BoxBody, context: StreamContext, guard: Option<CallGuard>) -> Self {
        let cancelled = context.0.token.clone().cancelled_owned();
        Self {
            inner,
            context,
            cancelled,
            guard,
            done: false,
        }
    }
}

impl Body for LifecycleBody {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        task: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Status>>> {
        let this = self.project();
        if *this.done {
            return Poll::Ready(None);
        }
        if this.cancelled.poll(task).is_ready() {
            *this.done = true;
            *this.inner = empty_body();
            let status = this
                .context
                .status()
                .map(|status| status.status.clone())
                .filter(|status| status.code() != Code::Ok)
                .unwrap_or_else(|| this.context.cancellation_status());
            return if this.guard.is_some() && this.context.role() == volo::context::Role::Server {
                Poll::Ready(Some(status.to_header_map().map(Frame::trailers)))
            } else {
                // On the client this is a local failure, not a peer Trailer.
                // Preserve its source for old third-party decoders as well.
                Poll::Ready(Some(Err(status)))
            };
        }
        let mut polled = this
            .context
            .sync_scope(|| Pin::new(&mut *this.inner).poll_frame(task));
        match &mut polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(trailer) = frame.trailers_mut() {
                    *this.done = true;
                    if this.guard.is_some() {
                        if this.context.role() == volo::context::Role::Server {
                            let mut defaults = this.context.response_metadata(true).into_headers();
                            defaults.extend(std::mem::take(trailer));
                            *trailer = defaults;
                            if let Err(status) =
                                super::metadata::write_backward(this.context, trailer)
                            {
                                *trailer = status.to_header_map().unwrap_or_default();
                            }
                        }
                        this.context
                            .on_trailer(MetadataMap::from_headers(trailer.clone()));
                        let status = Status::from_header_map(trailer)
                            .unwrap_or_else(|| Status::internal("missing final grpc-status"));
                        if status.code() == Code::Ok && this.context.has_response_decoder() {
                            // The decoder still has to validate framing, cardinality
                            // and the final status before a successful finish.
                            this.guard.as_mut().unwrap().disarm();
                        } else {
                            this.context.finish(status);
                        }
                    }
                }
            }
            Poll::Ready(Some(Err(status))) => {
                *this.done = true;
                this.context.on_io_error(status);
                this.context.finish(status.clone());
            }
            Poll::Ready(None) => {
                *this.done = true;
                if this.guard.is_some() {
                    if this.context.has_response_decoder() {
                        this.guard.as_mut().unwrap().disarm();
                    } else {
                        this.context
                            .finish(Status::internal("missing final grpc-status"));
                    }
                }
            }
            Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.done || self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;
    use http_body_util::StreamBody;
    use volo::context::Role;

    use super::*;
    use crate::{
        codec::{
            CodecLimits, Decoder,
            decode::{Kind, RecvStream},
        },
        message::RpcMode,
    };

    struct BytesDecoder;
    impl Decoder for BytesDecoder {
        type Item = Bytes;
        type Error = Status;
        fn decode(&mut self, bytes: Bytes) -> Result<Option<Bytes>, Status> {
            Ok(Some(bytes))
        }
    }

    #[tokio::test]
    async fn local_cancel_does_not_become_a_peer_status_for_legacy_decoders() {
        let cx = StreamContext::new("/test/Call", None, Role::Client, None);
        let body = response(empty_body(), CallGuard::new(cx.clone()));
        // Third-party EntryMessage implementations may use the old constructor,
        // without the new decoder context option.
        let mut input = RecvStream::with_decoder(
            body,
            Kind::Response(http::StatusCode::OK),
            None,
            BytesDecoder,
        );
        cx.cancel();
        let status = input.message().await.unwrap_err();
        assert_eq!(status.code(), Code::Cancelled);
        assert!(
            status.raw_grpc_code().is_none(),
            "locally generated cancellation is not a remote grpc-status"
        );
    }

    #[tokio::test]
    async fn final_success_waits_for_framing_cardinality_and_status_validation() {
        let valid = b"\0\0\0\0\x01x";
        let mut partial = valid.to_vec();
        partial.extend_from_slice(b"\0\0");
        let mut multiple = valid.to_vec();
        multiple.extend_from_slice(valid);
        for (payload, mode, wire_code, expected) in [
            (valid.to_vec(), RpcMode::Bidirectional, Code::Ok, Code::Ok),
            (partial, RpcMode::Bidirectional, Code::Ok, Code::Internal),
            (multiple, RpcMode::Unary, Code::Ok, Code::Internal),
            (vec![], RpcMode::Unary, Code::Ok, Code::Internal),
            (
                vec![0, 0, 0, 1, 0],
                RpcMode::Bidirectional,
                Code::Ok,
                Code::ResourceExhausted,
            ),
            (valid.to_vec(), RpcMode::Unary, Code::Aborted, Code::Aborted),
        ] {
            let cx = StreamContext::new("/test/Call", Some(mode), Role::Client, None);
            let frames = vec![
                Ok::<_, Status>(Frame::data(Bytes::from(payload))),
                Ok(Frame::trailers(
                    Status::new(wire_code, "wire").to_header_map().unwrap(),
                )),
            ];
            let body = response(
                boxed(StreamBody::new(stream::iter(frames))),
                CallGuard::new(cx.clone()),
            );
            let mut messages = RecvStream::with_decoder(
                body,
                Kind::Response(http::StatusCode::OK),
                None,
                BytesDecoder,
            )
            .with_limits(CodecLimits {
                max_receive_message_size: 8,
                ..CodecLimits::default()
            })
            .with_context(Some(cx.clone()));
            let actual = loop {
                match messages.message().await {
                    Ok(Some(_)) => {}
                    Ok(None) => break Code::Ok,
                    Err(status) => break status.code(),
                }
            };
            assert_eq!(actual, expected);
            assert_eq!(cx.status().unwrap().status.code(), expected);
        }
    }
}
