use bytes::{Bytes, BytesMut};
use futures::future::poll_fn;
use http_body::Body;

use crate::Status;

/// Preserve whether an error came from the producer, an observed peer reset,
/// or an H2 write. Converting a reset Reason to h2::Error loses that provenance.
pub(crate) enum SendBodyError {
    Producer(Status),
    Reset(h2::Reason),
    Write(h2::Error),
    Trailers(h2::Error),
    Closed,
}

impl SendBodyError {
    /// A peer can finish a response before the upload is half-closed. Only
    /// transport closure may defer to that response; producer errors and bad
    /// trailer headers remain errors. The receiver still validates final status.
    pub(crate) fn can_finish_from_response(&self) -> bool {
        match self {
            Self::Reset(reason) => *reason == h2::Reason::NO_ERROR,
            Self::Write(error) => {
                (error.is_reset() && error.reason() == Some(h2::Reason::NO_ERROR))
                    // These valid send/poll operations have no caller-supplied
                    // headers. A User error here means the stream is inactive.
                    || (error.reason().is_none() && !error.is_io())
            }
            Self::Trailers(error) => {
                error.is_reset() && error.reason() == Some(h2::Reason::NO_ERROR)
            }
            Self::Closed => true,
            Self::Producer(_) => false,
        }
    }

    pub(crate) fn into_status(self) -> Status {
        match self {
            Self::Producer(status) => status,
            Self::Reset(reason) => h2_error(reason.into()),
            Self::Write(error) | Self::Trailers(error) => h2_error(error),
            Self::Closed => Status::unavailable("H2 send direction closed"),
        }
    }
}

/// Reset is polled even while a producer or the send window is pending.
pub(crate) async fn send_body<B>(
    send: &mut h2::SendStream<Bytes>,
    body: &mut B,
    end: &mut bool,
    context: Option<&crate::streaming::StreamContext>,
) -> Result<(), SendBodyError>
where
    B: Body<Data = Bytes, Error = Status> + Unpin,
{
    if *end {
        return Ok(());
    }
    let mut pending = None;
    let mut eof = false;
    loop {
        let frame = if let Some(frame) = pending.take() {
            Some(frame)
        } else {
            poll_fn(|task| {
                if let std::task::Poll::Ready(reset) = send.poll_reset(task) {
                    return std::task::Poll::Ready(Some(Err(
                        reset.map_or_else(SendBodyError::Write, SendBodyError::Reset)
                    )));
                }
                std::pin::Pin::new(&mut *body)
                    .poll_frame(task)
                    .map(|frame| frame.map(|frame| frame.map_err(SendBodyError::Producer)))
            })
            .await
        };
        let Some(frame) = frame else {
            break;
        };
        let frame = frame?;
        match frame.into_data() {
            Ok(mut data) => {
                while !data.is_empty() {
                    // Reserve a bounded coalescing window for small fragments.
                    // Return unused capacity after enqueue so other streams are not starved.
                    send.reserve_capacity(if data.len() <= 1024 {
                        16 * 1024
                    } else {
                        data.len()
                    });
                    let mut waiting = None;
                    let capacity = poll_fn(|task| {
                        if let std::task::Poll::Ready(reset) = send.poll_reset(task) {
                            return std::task::Poll::Ready(Some(Err(
                                reset.map_or_else(SendBodyError::Write, SendBodyError::Reset)
                            )));
                        }
                        let capacity = send.poll_capacity(task);
                        if capacity.is_pending()
                            && waiting.is_none()
                            && context.is_some_and(|cx| cx.has_hook())
                        {
                            waiting = Some(std::time::Instant::now());
                        }
                        capacity.map(|capacity| {
                            capacity.map(|capacity| capacity.map_err(SendBodyError::Write))
                        })
                    })
                    .await;
                    if let (Some(started), Some(context)) = (waiting, context) {
                        context.on_flow_control(started.elapsed());
                    }
                    let capacity = capacity.ok_or(SendBodyError::Closed)??;
                    if capacity == 0 {
                        continue;
                    }
                    // Body lookahead can publish completion or an error. It
                    // must never precede a window wait for the current bytes.
                    if capacity >= data.len() {
                        // Peek only immediately ready frames: waiting for another message
                        // would deadlock request/response streaming. Large slices keep
                        // their original allocation; only adjacent small fragments copy.
                        let mut combined: Option<BytesMut> = None;
                        for _ in 0..64 {
                            let next = poll_fn(|task| {
                                std::task::Poll::Ready(
                                    std::pin::Pin::new(&mut *body).poll_frame(task),
                                )
                            })
                            .await;
                            match next {
                                std::task::Poll::Pending => break,
                                std::task::Poll::Ready(None) => {
                                    eof = true;
                                    break;
                                }
                                std::task::Poll::Ready(Some(Ok(frame)))
                                    if frame.data_ref().is_some_and(|bytes| {
                                        data.len() <= 1024
                                            && bytes.len() <= 1024
                                            && combined
                                                .as_ref()
                                                .map_or(data.len(), |bytes| bytes.len())
                                                + bytes.len()
                                                <= capacity.min(16 * 1024)
                                    }) =>
                                {
                                    let buffer = combined.get_or_insert_with(|| {
                                        let mut buffer = BytesMut::with_capacity(2048);
                                        buffer.extend_from_slice(&data);
                                        buffer
                                    });
                                    buffer.extend_from_slice(&frame.into_data().unwrap());
                                }
                                std::task::Poll::Ready(Some(frame)) => {
                                    pending = Some(frame.map_err(SendBodyError::Producer));
                                    break;
                                }
                            }
                        }
                        if let Some(combined) = combined {
                            data = combined.freeze();
                        }
                    }
                    let size = capacity.min(data.len());
                    let last = eof && size == data.len();
                    send.send_data(data.split_to(size), last)
                        .map_err(SendBodyError::Write)?;
                    if last {
                        *end = true;
                        return Ok(());
                    }
                }
                send.reserve_capacity(0);
                if eof {
                    break;
                }
            }
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers() {
                    if let Some(context) = context.filter(|cx| {
                        cx.role() == volo::context::Role::Server && cx.defers_response_commit()
                    }) {
                        context
                            .commit_response(trailers, |trailers| {
                                send.send_trailers(trailers).map_err(|error| {
                                    h2_error_with_phase(error, volo::streaming::IoPhase::Write)
                                })
                            })
                            .map_err(SendBodyError::Producer)?;
                    } else {
                        send.send_trailers(trailers)
                            .map_err(SendBodyError::Trailers)?;
                    }
                    *end = true;
                    return Ok(());
                }
            }
        }
    }
    send.send_data(Bytes::new(), true)
        .map_err(SendBodyError::Write)?;
    *end = true;
    Ok(())
}

pub(crate) fn h2_error(error: h2::Error) -> Status {
    Status::from_error(Box::new(error))
}

pub(crate) fn h2_error_with_phase(error: h2::Error, phase: volo::streaming::IoPhase) -> Status {
    if error.is_io() {
        Status::from(volo::streaming::RpcError::Transport {
            source: std::sync::Arc::new(std::io::Error::other(error)),
            phase,
            committed: true,
        })
    } else {
        h2_error(error)
    }
}

#[cfg(test)]
mod tests {
    use futures::{Stream, StreamExt};
    use http_body::Frame;
    use http_body_util::StreamBody;

    use super::*;

    async fn response(
        frames: impl Stream<Item = Result<Frame<Bytes>, Status>> + Send + 'static,
    ) -> (
        h2::RecvStream,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut connection = h2::server::handshake(server).await.unwrap();
            let (_, mut respond) = connection.accept().await.unwrap().unwrap();
            let mut send = respond
                .send_response(http::Response::new(()), false)
                .unwrap();
            let sending = tokio::spawn(async move {
                let mut body = StreamBody::new(Box::pin(frames));
                assert!(
                    send_body(&mut send, &mut body, &mut false, None)
                        .await
                        .is_ok()
                );
            });
            while connection.accept().await.is_some() {}
            sending.await.unwrap();
        });
        let (mut client, connection) = h2::client::handshake(client).await.unwrap();
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let (response, _) = client.send_request(http::Request::new(()), true).unwrap();
        (response.await.unwrap().into_body(), driver, server)
    }

    #[tokio::test]
    async fn small_ready_fragments_share_one_data_frame_and_final_end_stream() {
        let (mut body, driver, server) = response(futures::stream::iter([
            Ok(Frame::data(Bytes::from_static(b"abc"))),
            Ok(Frame::data(Bytes::from_static(b"def"))),
            Ok(Frame::data(Bytes::from_static(b"ghi"))),
        ]))
        .await;
        assert_eq!(body.data().await.unwrap().unwrap(), "abcdefghi");
        assert!(body.data().await.is_none(), "no separate empty EOS DATA");
        driver.abort();
        server.abort();
    }

    #[tokio::test]
    async fn lookahead_does_not_wait_for_the_next_message_or_drop_trailers() {
        let (release, released) = tokio::sync::oneshot::channel();
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
        let frames = futures::stream::iter([Ok(Frame::data(Bytes::from_static(b"first")))]).chain(
            futures::stream::once(async move {
                released.await.unwrap();
                Ok(Frame::trailers(trailers))
            }),
        );
        let (mut body, driver, server) = response(frames).await;
        let first = tokio::time::timeout(std::time::Duration::from_secs(1), body.data())
            .await
            .unwrap();
        assert_eq!(first.unwrap().unwrap(), "first");
        release.send(()).unwrap();
        assert!(body.data().await.is_none());
        assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
        driver.abort();
        server.abort();
    }
}
