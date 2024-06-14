use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::{BufMut, Bytes, BytesMut};
use futures::Stream;
use http::{header, HeaderValue};
use http_body::Frame;
use paste::paste;
use pin_project::pin_project;
use tokio::time::{Instant, Sleep};

use super::IntoResponse;
use crate::{body::Body, error::BoxError, response::ServerResponse};

/// Response of [SSE][sse] (Server-Sent Events), inclusing a stream with SSE [`Event`]s.
///
/// [sse]: https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events
pub struct Sse<S> {
    stream: S,
    keep_alive: Option<KeepAlive>,
}

impl<S> Sse<S> {
    /// Create a new SSE response with the given stream of [`Event`]s.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            keep_alive: None,
        }
    }

    /// Configure a [`KeepAlive`] for sending keep-alive messages.
    pub fn keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }
}

impl<S, E> IntoResponse for Sse<S>
where
    S: Stream<Item = Result<Event, E>> + Send + Sync + 'static,
    E: Into<BoxError>,
{
    fn into_response(self) -> ServerResponse {
        ServerResponse::builder()
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime::TEXT_EVENT_STREAM.essence_str()).expect("infallible"),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from_body(SseBody {
                stream: self.stream,
                keep_alive: self.keep_alive.map(KeepAliveStream::new),
            }))
            .expect("infallible")
    }
}

#[pin_project]
struct SseBody<S> {
    #[pin]
    stream: S,
    #[pin]
    keep_alive: Option<KeepAliveStream>,
}

impl<S, E> http_body::Body for SseBody<S>
where
    S: Stream<Item = Result<Event, E>>,
{
    type Data = Bytes;
    type Error = E;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        // Firstly, we should poll SSE stream
        match this.stream.poll_next(cx) {
            Poll::Pending => {
                // If the SSE stream is unavailable, poll the keep-alive stream
                if let Some(keep_alive) = this.keep_alive.as_pin_mut() {
                    keep_alive.poll_event(cx).map(|e| Some(Ok(Frame::data(e))))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(Some(Ok(event))) => {
                // The SSE stream is available, reset deadline of keep-alive stream
                if let Some(keep_alive) = this.keep_alive.as_pin_mut() {
                    keep_alive.reset();
                }
                Poll::Ready(Some(Ok(Frame::data(event.finalize()))))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
        }
    }
}

/// Message of [`Sse`]. Each event has some lines of specified fields including `event`, `data`,
/// `id`, `retry` and comment.
#[must_use]
#[derive(Default)]
pub struct Event {
    buffer: BytesMut,
    flags: EventFlags,
}

impl Event {
    const DATA: &'static str = "data";
    const EVENT: &'static str = "event";
    const ID: &'static str = "id";
    const RETRY: &'static str = "retry";

    pub fn new() -> Self {
        Self::default()
    }

    /// Set the event field (`event: <event-name>`) for the event message.
    ///
    /// # Panics
    ///
    /// - Panics if the event field has already set.
    /// - Panics if the event name contains `\r` or `\n`.
    pub fn event<T>(mut self, event: T) -> Self
    where
        T: AsRef<str>,
    {
        assert!(
            !self.flags.contains_event(),
            "Each `Event` cannot have more than one event field",
        );
        self.flags.set_event();

        self.field(Self::EVENT, event.as_ref());

        self
    }

    /// Set the data field(s) (`data: <content>`) for the event message.
    ///
    /// Each line of contents will be added `data: ` prefix when sending.
    ///
    /// # Panics
    ///
    /// - Panics if the data field has already set through [`Self::data`] or [`Self::json`].
    pub fn data<T>(mut self, data: T) -> Self
    where
        T: AsRef<str>,
    {
        assert!(
            !self.flags.contains_data(),
            "Each `Event` cannot have more than one data",
        );
        self.flags.set_data();

        for line in memchr_split(b'\n', data.as_ref().as_bytes()) {
            self.field(Self::DATA, line);
        }

        self
    }

    /// Set the data field (`data: <content>`) by serialized data for the event message.
    ///
    /// Each line of contents will be added `data: ` prefix when sending.
    ///
    /// # Panics
    ///
    /// - Panics if the data field has already set through [`Self::data`] or [`Self::json`].
    #[cfg(feature = "__json")]
    pub fn json<T>(mut self, data: &T) -> Result<Self, crate::json::Error>
    where
        T: serde::Serialize,
    {
        assert!(
            !self.flags.contains_data(),
            "Each `Event` cannot have more than one data",
        );
        self.flags.set_data();

        self.buffer.extend_from_slice(Self::DATA.as_bytes());
        self.buffer.put_u8(b':');
        self.buffer.put_u8(b' ');

        let mut writer = self.buffer.writer();
        crate::json::serialize_to_writer(&mut writer, data)?;
        self.buffer = writer.into_inner();

        Ok(self)
    }

    /// Set the id field (`id: <id>`) for the event message.
    ///
    /// # Panics
    ///
    /// - Panics if the id field has already set.
    /// - Panics if the id contains `\r` or `\n`.
    pub fn id<T>(mut self, id: T) -> Self
    where
        T: AsRef<str>,
    {
        assert!(
            !self.flags.contains_id(),
            "Each `Event` cannot have more than one id",
        );
        self.flags.set_id();

        self.field(Self::ID, id.as_ref().as_bytes());

        self
    }

    /// Set the retry field (`retry: <timeout>`) for the event message.
    ///
    /// # Panics
    ///
    /// - Panics if the timeout field has already set.
    pub fn retry(mut self, timeout: Duration) -> Self {
        assert!(
            !self.flags.contains_retry(),
            "Each `Event` cannot have more than one retry field",
        );
        self.flags.set_retry();

        self.buffer.extend_from_slice(Self::RETRY.as_bytes());
        self.buffer.put_u8(b':');
        self.buffer.put_u8(b' ');
        self.buffer
            .extend_from_slice(itoa::Buffer::new().format(timeout.as_millis()).as_bytes());
        self.buffer.put_u8(b'\n');

        self
    }

    /// Add a comment field (`: <comment-text>`).
    ///
    /// # Panics
    ///
    /// - Panics if the comment text contains `\r` or `\n`.
    pub fn comment<T>(mut self, comment: T) -> Self
    where
        T: AsRef<str>,
    {
        self.field("", comment.as_ref().as_bytes());
        self
    }

    fn field<V>(&mut self, key: &'static str, val: V)
    where
        V: AsRef<[u8]>,
    {
        let val = val.as_ref();
        assert_eq!(
            memchr::memchr2(b'\r', b'\n', val),
            None,
            "Field should not contain `\\r` or `\\n`",
        );

        self.buffer.extend_from_slice(key.as_bytes());
        self.buffer.put_u8(b':');
        self.buffer.put_u8(b' ');
        self.buffer.extend_from_slice(val);
        self.buffer.put_u8(b'\n');
    }

    fn finalize(mut self) -> Bytes {
        self.buffer.put_u8(b'\n');
        self.buffer.freeze()
    }
}

/// Configure a interval to send a message for keeping SSE connection alive.
pub struct KeepAlive {
    event: Bytes,
    max_interval: Duration,
}

impl KeepAlive {
    /// Create a new `KeepAlive` with an empty comment as message.
    pub fn new() -> Self {
        Self {
            event: Bytes::from_static(b":\n\n"),
            max_interval: Duration::from_secs(15),
        }
    }

    /// Set the interval between keep-alive messages.
    ///
    /// Default is 15 seconds.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.max_interval = interval;
        self
    }

    /// Set the comment text for the keep-alive message.
    ///
    /// Default is an empty comment.
    ///
    /// # Panics
    ///
    /// - Panics if the comment text contains `\r` or `\n`.
    pub fn text<T>(mut self, text: T) -> Self
    where
        T: AsRef<str>,
    {
        self.event = Event::new().comment(text).finalize();
        self
    }

    /// Set the event of keep-alive message.
    ///
    /// Default is an empty comment.
    pub fn event(mut self, event: Event) -> Self {
        self.event = event.finalize();
        self
    }
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self::new()
    }
}

#[pin_project]
struct KeepAliveStream {
    keep_alive: KeepAlive,
    #[pin]
    alive_timer: Sleep,
}

impl KeepAliveStream {
    fn new(keep_alive: KeepAlive) -> Self {
        Self {
            alive_timer: tokio::time::sleep(keep_alive.max_interval),
            keep_alive,
        }
    }

    fn reset(self: Pin<&mut Self>) {
        let this = self.project();
        this.alive_timer
            .reset(Instant::now() + this.keep_alive.max_interval);
    }

    fn poll_event(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Bytes> {
        let this = self.as_mut().project();

        if this.alive_timer.poll(cx).is_pending() {
            return Poll::Pending;
        }

        let event = self.keep_alive.event.clone();
        self.reset();

        Poll::Ready(event)
    }
}

// Copied from `axum/src/response/sse.rs`
fn memchr_split(needle: u8, haystack: &[u8]) -> MemchrSplit<'_> {
    MemchrSplit {
        needle,
        haystack: Some(haystack),
    }
}

struct MemchrSplit<'a> {
    needle: u8,
    haystack: Option<&'a [u8]>,
}

impl<'a> Iterator for MemchrSplit<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        let haystack = self.haystack?;
        if let Some(pos) = memchr::memchr(self.needle, haystack) {
            let (front, back) = haystack.split_at(pos);
            self.haystack = Some(&back[1..]);
            Some(front)
        } else {
            self.haystack.take()
        }
    }
}

macro_rules! define_bitflag {
    (struct $name:ident($type:ty) { $( $flag:ident = $val:tt, )+ }) => {
        #[derive(Default)]
        struct $name($type);

        impl $name {
            $(
                paste! {
                    const [<$flag:upper>]: $type = $val;

                    #[inline]
                    fn [<set_ $flag:lower>](&mut self) {
                        self.0 |= Self::[<$flag:upper>];
                    }

                    #[inline]
                    fn [<contains_ $flag:lower>](&self) -> bool {
                        self.0 & Self::[<$flag:upper>] == Self::[<$flag:upper>]
                    }
                }
            )+
        }
    }
}

define_bitflag! {
    struct EventFlags(u8) {
        DATA    = 0b0001,
        EVENT   = 0b0010,
        ID      = 0b0100,
        RETRY   = 0b1000,
    }
}

#[cfg(test)]
mod sse_tests {
    use std::{convert::Infallible, time::Duration};

    use ahash::AHashMap;
    use async_stream::stream;
    use faststr::FastStr;
    use futures::{stream, Stream, StreamExt};
    use http::{header, method::Method};
    use http_body_util::BodyExt;

    use super::{memchr_split, Event, KeepAlive, Sse};
    use crate::{
        body::Body,
        server::route::{any, MethodRouter},
    };

    impl Event {
        fn into_string(self) -> String {
            unsafe { String::from_utf8_unchecked(self.finalize().to_vec()) }
        }
    }

    #[test]
    fn event_build() {
        // Empty event
        assert_eq!(Event::new().into_string(), "\n");

        // Single field
        assert_eq!(
            Event::new().event("sse-event").into_string(),
            "event: sse-event\n\n",
        );
        assert_eq!(
            Event::new().data("text-data").into_string(),
            "data: text-data\n\n",
        );
        assert_eq!(Event::new().id("seq-001").into_string(), "id: seq-001\n\n");
        assert_eq!(
            Event::new().retry(Duration::from_secs(1)).into_string(),
            "retry: 1000\n\n",
        );
        assert_eq!(
            Event::new().comment("comment").into_string(),
            ": comment\n\n",
        );

        // Multi-line data
        assert_eq!(
            Event::new().data("114\n514\n1919\n810").into_string(),
            "data: 114\ndata: 514\ndata: 1919\ndata: 810\n\n",
        );

        // Multi-field event
        assert_eq!(
            Event::new()
                .event("ping")
                .data("hello\nworld")
                .id("first")
                .retry(Duration::from_secs(15))
                .comment("test comment")
                .into_string(),
            "event: ping\ndata: hello\ndata: world\nid: first\nretry: 15000\n: test comment\n\n",
        );
    }

    #[test]
    #[should_panic]
    fn multi_event() {
        let _ = Event::new().event("ping").event("pong").into_string();
    }

    #[test]
    #[should_panic]
    fn multi_data() {
        let _ = Event::new().data("data1").data("data2").into_string();
    }

    #[test]
    #[should_panic]
    fn multi_id() {
        let _ = Event::new().id("ping-1").id("ping-2").into_string();
    }

    #[test]
    #[should_panic]
    fn multi_retry() {
        let _ = Event::new()
            .retry(Duration::from_secs(1))
            .retry(Duration::from_secs(1))
            .into_string();
    }

    #[test]
    // This will not panic
    fn multi_comment() {
        assert_eq!(
            Event::new()
                .comment("114514")
                .comment("1919810")
                .into_string(),
            ": 114514\n: 1919810\n\n",
        );
    }

    #[test]
    // Copied from `axum/src/response/sse.rs`
    fn memchr_splitting() {
        assert_eq!(
            memchr_split(2, &[]).collect::<Vec<_>>(),
            [&[]] as [&[u8]; 1]
        );
        assert_eq!(
            memchr_split(2, &[2]).collect::<Vec<_>>(),
            [&[], &[]] as [&[u8]; 2]
        );
        assert_eq!(
            memchr_split(2, &[1]).collect::<Vec<_>>(),
            [&[1]] as [&[u8]; 1]
        );
        assert_eq!(
            memchr_split(2, &[1, 2]).collect::<Vec<_>>(),
            [&[1], &[]] as [&[u8]; 2]
        );
        assert_eq!(
            memchr_split(2, &[2, 1]).collect::<Vec<_>>(),
            [&[], &[1]] as [&[u8]; 2]
        );
        assert_eq!(
            memchr_split(2, &[1, 2, 2, 1]).collect::<Vec<_>>(),
            [&[1], &[], &[1]] as [&[u8]; 3]
        );
    }

    fn parse_event(s: &str) -> AHashMap<String, String> {
        let mut res: AHashMap<String, String> = AHashMap::new();

        for line in s.split('\n') {
            if line.is_empty() {
                continue;
            }
            let Some(pos) = line.find(": ") else {
                continue;
            };
            // key: value
            // 0123456789
            //    |
            //   pos
            //
            // key: [..pos)
            // val: [pos+2..)
            let mut key = line[..pos].to_owned();
            if key.is_empty() {
                key.push_str("comment");
            }
            let val = &line[pos + 2..];
            if res.contains_key(&key) {
                res.get_mut(&key).unwrap().push('\n');
            } else {
                res.insert(key.clone(), Default::default());
            }
            res.get_mut(&key).unwrap().push_str(val);
        }

        res
    }

    async fn poll_event(body: &mut Body) -> AHashMap<String, String> {
        let data = body
            .frame()
            .await
            .expect("No frame found")
            .expect("Failed to pull frame")
            .into_data()
            .expect("Frame is not data");
        let s = FastStr::from_bytes(data).expect("Frame data is not a valid string");
        parse_event(&s)
    }

    #[tokio::test]
    async fn simple_event() {
        async fn sse_handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
            Sse::new(
                stream::iter(vec![
                    Event::new().event("ping").data("-"),
                    Event::new().event("ping").data("biu"),
                    Event::new()
                        .event("pong")
                        .id("pong")
                        .retry(Duration::from_secs(1))
                        .comment(""),
                ])
                .map(Ok),
            )
        }
        let router: MethodRouter<Option<Body>> = any(sse_handler);
        let resp = router.call_route(Method::GET, None).await;
        let (parts, mut body) = resp.into_parts();
        assert_eq!(
            parts
                .headers
                .get(header::CONTENT_TYPE)
                .expect("`Content-Type` does not exist")
                .to_str()
                .expect("`Content-Type` is not a valid string"),
            mime::TEXT_EVENT_STREAM.essence_str(),
        );
        assert_eq!(
            parts
                .headers
                .get(header::CACHE_CONTROL)
                .expect("`Cache-Control` does not exist")
                .to_str()
                .expect("`Cache-Control` is not a valid string"),
            "no-cache",
        );

        // Event 1
        let event = poll_event(&mut body).await;
        assert_eq!(event.len(), 2);
        assert_eq!(event.get("event").unwrap(), "ping");
        assert_eq!(event.get("data").unwrap(), "-");

        // Event 2
        let event = poll_event(&mut body).await;
        assert_eq!(event.len(), 2);
        assert_eq!(event.get("event").unwrap(), "ping");
        assert_eq!(event.get("data").unwrap(), "biu");

        // Event 3
        let event = poll_event(&mut body).await;
        assert_eq!(event.len(), 4);
        assert_eq!(event.get("event").unwrap(), "pong");
        assert_eq!(event.get("id").unwrap(), "pong");
        assert_eq!(event.get("retry").unwrap(), "1000");
        assert_eq!(event.get("comment").unwrap(), "");
    }

    #[tokio::test]
    async fn keep_alive() {
        async fn sse_handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
            let stream = stream! {
                loop {
                    yield Ok(Event::new().event("ping"));
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            };

            Sse::new(stream).keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(1))
                    .text("do not kill me"),
            )
        }

        let router: MethodRouter<Option<Body>> = any(sse_handler);
        let resp = router.call_route(Method::GET, None).await;
        let (_, mut body) = resp.into_parts();

        // The first message is event
        let event_fields = poll_event(&mut body).await;
        assert_eq!(event_fields.get("event").unwrap(), "ping");

        // Then 4 keep-alive messages
        for _ in 0..4 {
            let event_fields = poll_event(&mut body).await;
            assert_eq!(event_fields.get("comment").unwrap(), "do not kill me");
        }

        // After 5 seconds, event is coming
        let event_fields = poll_event(&mut body).await;
        assert_eq!(event_fields.get("event").unwrap(), "ping");
    }

    #[tokio::test]
    async fn keep_alive_ends_when_the_stream_ends() {
        async fn sse_handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
            let stream = stream! {
                // Sleep 5 seconds and send only one event
                tokio::time::sleep(Duration::from_secs(5)).await;
                yield Ok(Event::new().event("ping"));
            };

            Sse::new(stream).keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(1))
                    .text("do not kill me"),
            )
        }

        let router: MethodRouter<Option<Body>> = any(sse_handler);
        let resp = router.call_route(Method::GET, None).await;
        let (_, mut body) = resp.into_parts();

        // 4 comments before event
        for _ in 0..4 {
            let event_fields = poll_event(&mut body).await;
            assert_eq!(event_fields.get("comment").unwrap(), "do not kill me");
        }

        // Event is coming
        let event_fields = poll_event(&mut body).await;
        assert_eq!(event_fields.get("event").unwrap(), "ping");

        // Stream finished
        assert!(body.frame().await.is_none());
    }
}
