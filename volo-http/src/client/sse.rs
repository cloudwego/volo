//! This module provides [`SseReader`] for consuming SSE streams from a server,
//! mirroring the server-side [`Sse`] response type in `server::response::sse`.
//!
//! [`Sse`]: crate::server::response::sse::Sse
use std::{pin::Pin, time::Duration};

use bytes::Bytes;
use http_body::Body;
use http_body_util::BodyExt;

use crate::{error::BoxError, response::Response};

/// Error message when the response body is not a valid SSE stream.
const ERR_INVALID_CONTENT_TYPE: &str = "Content-Type returned by server is NOT text/event-stream";

// Constants for event field names in the SSE stream. Used for parsing incoming events.
const DATA: &str = "data";
const EVENT: &str = "event";
const ID: &str = "id";
const RETRY: &str = "retry";

/// Bitflags tracking which fields have been set on the current event being parsed.
///
/// An event is only dispatched when at least one flag is set (`bitset != 0`).
/// Comments do not set any flag and therefore do not trigger dispatch on their own.
const BIT_DATA: u8 = 0b0001;
const BIT_EVENT: u8 = 0b0010;
const BIT_ID: u8 = 0b0100;
const BIT_RETRY: u8 = 0b1000;

/// Extension trait adding [`SseExt::into_sse`] to [`Response`].
pub trait SseExt<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    /// Consume the response and return an [`SseReader`].
    ///
    /// Returns an error if the `Content-Type` is not `text/event-stream`.
    fn into_sse(self) -> Result<SseReader<B>, BoxError>;
}

impl<B> SseExt<B> for Response<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    fn into_sse(self) -> Result<SseReader<B>, BoxError> {
        SseReader::into_sse(self)
    }
}

/// A parsed SSE event received from the server.
#[derive(Debug, Default, Clone)]
pub struct SseEvent {
    /// Multiple `data:` lines are joined with `\n`.
    pub data: Option<String>,
    /// The event type (`event:` field). Defaults to `"message"` per the SSE spec.
    pub event: Option<String>,
    /// The event ID (`id:` field). `None` if not set or explicitly cleared.
    pub id: Option<String>,
    /// The retry duration (`retry:` field).
    pub retry: Option<Duration>,
}

impl SseEvent {
    /// Returns the event type. Defaults to `"message"` if not explicitly set.
    pub fn event(&self) -> &str {
        self.event.as_deref().unwrap_or("message")
    }

    /// Returns the event data, if any.
    pub fn data(&self) -> Option<&str> {
        self.data.as_deref()
    }

    /// Returns the event ID, if any.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the retry duration, if any.
    pub fn retry(&self) -> Option<Duration> {
        self.retry
    }
}

/// Internal accumulator for the event currently being parsed.
///
/// `bitset` tracks which fields have been set; an event is only
/// dispatched when `bitset != 0` (i.e. at least one real field was seen).
#[derive(Default)]
struct EventBuffer {
    /// Tracks which fields have been explicitly set on the current event.
    bitset: u8,
    data: String,
    event: Option<String>,
    id: Option<String>,
    retry: Option<Duration>,
}

impl EventBuffer {
    /// Clear all fields and reset the bitset to zero.
    fn reset(&mut self) {
        self.bitset = 0;
        self.data.clear();
        self.event = None;
        self.id = None;
        self.retry = None;
    }

    /// Returns true if at least one real field (data/event/id/retry) has been set.
    fn has_field(&self) -> bool {
        self.bitset != 0
    }

    /// Returns true if the `id:` field was explicitly set in this event.
    fn is_set_id(&self) -> bool {
        self.bitset & BIT_ID != 0
    }

    /// Consume the buffer into an `SseEvent`.
    fn dispatch(&mut self) -> SseEvent {
        let event = SseEvent {
            event: self.event.take(),
            data: if self.bitset & BIT_DATA != 0 {
                Some(std::mem::take(&mut self.data))
            } else {
                None
            },
            id: self.id.take().filter(|s| !s.is_empty()),
            retry: self.retry.take(),
        };
        self.reset();
        event
    }
}

/// A reader for SSE response body.
///
/// Wraps a streaming response body and parses it into [`SseEvent`]s.
pub struct SseReader<B> {
    body: B,
    /// Raw byte buffer accumulating bytes across body frames.
    buffer: Vec<u8>,
    /// The last event ID string, for use as `Last-Event-ID` on reconnection.
    /// Empty string means the server explicitly cleared it via `id:` with no value.
    /// Only updated when `id:` is present in the dispatched event.
    last_event_id: String,
    /// Whether this is the very first line of the stream, for BOM stripping.
    is_first_line: bool,
    /// Internal accumulator for the event currently being parsed.
    pending: EventBuffer,
}

impl<B> SseReader<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    /// Create a new SSE reader from an HTTP response.
    pub fn into_sse(resp: Response<B>) -> Result<Self, BoxError> {
        if !resp.status().is_success() {
            return Err(format!("Server returned error status: {}", resp.status()).into());
        }

        // Check that the Content-Type is text/event-stream
        let content_type = resp
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !content_type.starts_with(mime::TEXT_EVENT_STREAM.essence_str()) {
            return Err(ERR_INVALID_CONTENT_TYPE.into());
        }

        Ok(Self {
            body: resp.into_body(),
            buffer: Vec::new(),
            last_event_id: String::new(),
            is_first_line: true,
            pending: EventBuffer::default(),
        })
    }

    /// Returns the last event ID received, for use as `Last-Event-ID` on reconnection.
    ///
    /// Empty string means the server explicitly cleared it.
    pub fn last_event_id(&self) -> &str {
        &self.last_event_id
    }

    /// Read the next SSE event from the stream.
    ///
    /// Returns `Ok(Some(event))` when an event is ready, `Ok(None)` when the
    /// stream is exhausted, or `Err` on a transport or parse error.
    pub async fn read(&mut self) -> Result<Option<SseEvent>, BoxError> {
        loop {
            // ── 1. Drain all complete lines currently in the buffer ──────────
            while let Some(line) = self.next_line() {
                if let Some(event) = self.process_line(line)? {
                    return Ok(Some(event));
                }
            }

            // ── 2. Pull the next frame from the body ─────────────────────────
            match Pin::new(&mut self.body).frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.buffer.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => return Err(e.into()),
                None => {
                    // Body exhausted. Flush any unterminated last line by
                    // appending a synthetic newline, then do one final drain.
                    if !self.buffer.is_empty() {
                        self.buffer.push(b'\n');
                        while let Some(line) = self.next_line() {
                            if let Some(event) = self.process_line(line)? {
                                return Ok(Some(event));
                            }
                        }
                    }
                    // Flush any pending event that didn't end with a blank line.
                    if self.pending.has_field() {
                        return Ok(Some(self.dispatch_pending()));
                    }
                    return Ok(None);
                }
            }
        }
    }

    /// Extract the next complete line from `self.buffer`, handling all three
    /// spec-required line endings: CRLF, LF, and bare CR.
    ///
    /// Returns `None` when no complete line is available yet.
    fn next_line(&mut self) -> Option<String> {
        let pos = self.buffer.iter().position(|&b| b == b'\n' || b == b'\r')?;

        let terminator = self.buffer[pos];
        let mut line_bytes: Vec<u8> = self.buffer.drain(..pos).collect();

        // Consume the terminator itself.
        self.buffer.remove(0);

        // CRLF: consume the following LF so it isn't treated as a second line.
        if terminator == b'\r' && self.buffer.first() == Some(&b'\n') {
            self.buffer.remove(0);
        }

        // BOM stripping on the very first line of the stream.
        if self.is_first_line {
            self.is_first_line = false;
            if line_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                line_bytes.drain(..3);
            }
        }

        Some(String::from_utf8_lossy(&line_bytes).into_owned())
    }

    /// Process a single decoded line, updating `self.pending`.
    ///
    /// Returns `Some(event)` when a blank line triggers dispatch, `None` otherwise.
    fn process_line(&mut self, line: String) -> Result<Option<SseEvent>, BoxError> {
        if line.is_empty() {
            // Blank line → dispatch if any real field was seen.
            if self.pending.has_field() {
                return Ok(Some(self.dispatch_pending()));
            }
            // No real fields seen (e.g. leading blank lines or all-comment block).
            return Ok(None);
        }

        // Comment line (starts with ':'). Ignored per spec.
        if line.starts_with(':') {
            return Ok(None);
        }

        // Field line: split on first ':'.
        // If no colon, the whole line is the field name with an empty value.
        let (field, value) = match line.find(':') {
            Some(idx) => {
                // Strip exactly one leading space after ':', if present.
                let v = line[idx + 1..]
                    .strip_prefix(' ')
                    .unwrap_or(&line[idx + 1..]);
                (&line[..idx], v.to_string())
            }
            None => (line.as_str(), String::new()),
        };

        match field {
            DATA => {
                // Prepend '\n' when data already exists, then append.
                // This avoids a trailing-newline-strip step at dispatch time.
                if self.pending.bitset & BIT_DATA != 0 {
                    self.pending.data.push('\n');
                }
                self.pending.data.push_str(&value);
                self.pending.bitset |= BIT_DATA;
            }
            EVENT => {
                self.pending.event = Some(value);
                self.pending.bitset |= BIT_EVENT;
            }
            // Ignore if the value contains a NULL byte, per spec.
            ID if !value.contains('\0') => {
                self.pending.id = Some(value);
                self.pending.bitset |= BIT_ID;
            }
            RETRY => {
                // Parse as u64, ignore if not a valid integer.
                if let Ok(ms) = value.parse::<u64>() {
                    self.pending.retry = Some(Duration::from_millis(ms));
                    self.pending.bitset |= BIT_RETRY;
                }
            }
            _ => {} // Unknown fields are ignored per spec.
        }

        Ok(None)
    }

    /// Commit `last_event_id` and consume the pending buffer into an `SseEvent`.
    fn dispatch_pending(&mut self) -> SseEvent {
        // Only update last_event_id when `id:` was explicitly present,
        // including the empty-string case which clears it.
        if self.pending.is_set_id() {
            self.last_event_id = self.pending.id.as_deref().unwrap_or_default().to_owned();
        }
        self.pending.dispatch()
    }
}

#[cfg(test)]
mod sse_reader_tests {
    use std::time::Duration;

    use bytes::Bytes;
    use http::header;
    use http_body_util::Full;

    use super::SseReader;
    use crate::response::Response;

    fn make_response(body: &'static str) -> Response<Full<Bytes>> {
        Response::builder()
            .header(header::CONTENT_TYPE, mime::TEXT_EVENT_STREAM.essence_str())
            .body(Full::new(Bytes::from_static(body.as_bytes())))
            .unwrap()
    }

    #[test]
    fn rejects_wrong_content_type() {
        let resp = Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert!(SseReader::into_sse(resp).is_err());
    }

    #[test]
    fn rejects_missing_content_type() {
        let resp = Response::builder().body(Full::new(Bytes::new())).unwrap();
        assert!(SseReader::into_sse(resp).is_err());
    }

    #[tokio::test]
    async fn single_data_field() {
        let mut reader = SseReader::into_sse(make_response("data: hello\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
        assert_eq!(event.event(), "message");
        assert_eq!(event.id(), None);
        assert_eq!(event.retry(), None);
    }

    #[tokio::test]
    async fn single_event_field() {
        let mut reader = SseReader::into_sse(make_response("event: ping\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), None);
        assert_eq!(event.event(), "ping");
        assert_eq!(event.id(), None);
        assert_eq!(event.retry(), None);
    }

    #[tokio::test]
    async fn single_id_field() {
        let mut reader = SseReader::into_sse(make_response("id: 42\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), None);
        assert_eq!(event.event(), "message");
        assert_eq!(event.id(), Some("42"));
        assert_eq!(event.retry(), None);
    }

    #[tokio::test]
    async fn single_retry_field() {
        let mut reader = SseReader::into_sse(make_response("retry: 3000\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), None);
        assert_eq!(event.event(), "message");
        assert_eq!(event.id(), None);
        assert_eq!(event.retry(), Some(Duration::from_millis(3000)));
    }

    #[tokio::test]
    async fn multi_field_event() {
        let mut reader = SseReader::into_sse(make_response(
            "event: ping\ndata: hello\ndata: world\nid: first\nretry: 15000\n: test comment\n\n",
        ))
        .unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.event(), "ping");
        assert_eq!(event.data(), Some("hello\nworld"));
        assert_eq!(event.id(), Some("first"));
        assert_eq!(event.retry(), Some(Duration::from_millis(15000)));
    }

    #[tokio::test]
    async fn multiline_data() {
        let mut reader = SseReader::into_sse(make_response(
            "data: 114\ndata: 514\ndata: 1919\ndata: 810\n\n",
        ))
        .unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("114\n514\n1919\n810"));
        assert_eq!(event.event(), "message");
        assert_eq!(event.id(), None);
        assert_eq!(event.retry(), None);
    }

    #[tokio::test]
    async fn empty_data_field() {
        let mut reader = SseReader::into_sse(make_response("data:\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some(""));
        assert_eq!(event.event(), "message");
        assert_eq!(event.id(), None);
        assert_eq!(event.retry(), None);
    }

    #[tokio::test]
    async fn multiple_events() {
        let mut reader = SseReader::into_sse(make_response(
            "event: ping\ndata: -\n\nevent: pong\ndata: -\n\n",
        ))
        .unwrap();

        let e1 = reader.read().await.unwrap().unwrap();
        assert_eq!(e1.data(), Some("-"));
        assert_eq!(e1.event(), "ping");
        assert_eq!(e1.id(), None);
        assert_eq!(e1.retry(), None);

        let e2 = reader.read().await.unwrap().unwrap();
        assert_eq!(e2.data(), Some("-"));
        assert_eq!(e2.event(), "pong");
        assert_eq!(e2.id(), None);
        assert_eq!(e2.retry(), None);

        assert!(reader.read().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn returns_none_on_empty_stream() {
        let mut reader = SseReader::into_sse(make_response("")).unwrap();
        assert!(reader.read().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn returns_none_after_last_event() {
        let mut reader = SseReader::into_sse(make_response("data: hello\n\n")).unwrap();
        reader.read().await.unwrap().unwrap();
        assert!(reader.read().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn comments_are_ignored() {
        let mut reader =
            SseReader::into_sse(make_response(": ping\n: pong\n\ndata: hello\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
        assert_eq!(event.event(), "message");
        assert_eq!(event.id(), None);
        assert_eq!(event.retry(), None);
        assert!(reader.read().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn last_event_id_tracks_across_events() {
        let mut reader = SseReader::into_sse(make_response(
            "id: 1\ndata: a\n\ndata: b\n\nid: 3\ndata: c\n\n",
        ))
        .unwrap();

        reader.read().await.unwrap().unwrap();
        assert_eq!(reader.last_event_id(), "1");

        // Event with no id: last_event_id must not change.
        reader.read().await.unwrap().unwrap();
        assert_eq!(reader.last_event_id(), "1");

        reader.read().await.unwrap().unwrap();
        assert_eq!(reader.last_event_id(), "3");
    }

    #[tokio::test]
    async fn empty_id_clears_last_event_id() {
        let mut reader =
            SseReader::into_sse(make_response("id: 42\ndata: a\n\nid:\ndata: b\n\n")).unwrap();

        reader.read().await.unwrap().unwrap();
        assert_eq!(reader.last_event_id(), "42");

        // Empty id: explicitly clears last_event_id on the reader,
        // but the dispatched event normalises it to None.
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(reader.last_event_id(), "");
        assert_eq!(event.id(), None);
    }

    #[tokio::test]
    async fn retry_invalid_is_ignored() {
        let mut reader = SseReader::into_sse(make_response("retry: abc\ndata: hello\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
        assert_eq!(event.retry(), None);
    }

    #[tokio::test]
    async fn retry_with_suffix_is_ignored() {
        let mut reader =
            SseReader::into_sse(make_response("retry: 1000abc\ndata: hello\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
        assert_eq!(event.retry(), None);
    }

    #[tokio::test]
    async fn crlf_line_endings() {
        let mut reader =
            SseReader::into_sse(make_response("data: hello\r\ndata: world\r\n\r\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello\nworld"));
    }

    #[tokio::test]
    async fn bare_cr_line_endings() {
        let mut reader =
            SseReader::into_sse(make_response("data: hello\rdata: world\r\r")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello\nworld"));
    }

    #[tokio::test]
    async fn bom_stripped_on_first_line() {
        let mut body = vec![0xEF, 0xBB, 0xBF];
        body.extend_from_slice(b"data: hello\n\n");
        let resp = Response::builder()
            .header(header::CONTENT_TYPE, mime::TEXT_EVENT_STREAM.essence_str())
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let mut reader = SseReader::into_sse(resp).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
    }

    #[tokio::test]
    async fn unknown_field_is_ignored() {
        let mut reader =
            SseReader::into_sse(make_response("unknown: value\ndata: hello\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
    }

    #[tokio::test]
    async fn field_with_no_colon_is_ignored() {
        let mut reader =
            SseReader::into_sse(make_response("unknownfield\ndata: hello\n\n")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
    }

    #[tokio::test]
    async fn event_without_trailing_blank_line_is_flushed() {
        let mut reader = SseReader::into_sse(make_response("data: hello")).unwrap();
        let event = reader.read().await.unwrap().unwrap();
        assert_eq!(event.data(), Some("hello"));
    }
}
