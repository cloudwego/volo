use bytes::Bytes;
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
    while let Some(frame) = poll_fn(|task| {
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
    {
        let frame = frame?;
        match frame.into_data() {
            Ok(mut data) => {
                while !data.is_empty() {
                    send.reserve_capacity(data.len());
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
                    let size = capacity.min(data.len());
                    send.send_data(data.split_to(size), false)
                        .map_err(SendBodyError::Write)?;
                }
            }
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers() {
                    send.send_trailers(trailers)
                        .map_err(SendBodyError::Trailers)?;
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
