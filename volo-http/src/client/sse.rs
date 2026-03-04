//! SSE (Server-Sent Events) client support.
//!
//! This module provides [`SseReader`] for consuming SSE streams from a server,
//! mirroring the server-side [`Sse`] response type in `server::response::sse`.

use std::{pin::Pin, time::Duration};

use bytes::Bytes;
use http_body::Body;
use http_body_util::BodyExt;

use crate::error::BoxError;

/// Error message when the response body is not a valid SSE stream.
const ERR_INVALID_CONTENT_TYPE: &str = "Content-Type returned by server is NOT text/event-stream";

/// Constants for event field names in the SSE stream. Used for parsing incoming events.
const DATA: &'static str = "data";
const EVENT: &'static str = "event";
const ID: &'static str = "id";
const RETRY: &'static str = "retry";

/// A parsed SSE event received from the server.
#[derive(Debug, Default, Clone)]
pub struct SseEvent {
    /// The event type (`event:` field)
    event: Option<String>,
    /// The event data (`data:` field). Multi-line data is joined with `\n`.
    data: Option<String>,
    /// The event ID (`id:` field)
    id: Option<String>,
    /// The retry duration (`retry:` field)
    retry: Option<Duration>,
    /// Comment lines (`: comment`). Multiple comments per event are supported.
    comment: Option<Vec<String>>,
}

impl SseEvent {
    /// Returns the event type, if set.
    ///
    /// Corresponds to the `event:` field in the SSE stream.
    pub fn event(&self) -> Option<&str> {
        self.event.as_deref()
    }

    /// Returns the event data, if set.
    ///
    /// Corresponds to the `data:` field(s) in the SSE stream.
    /// Multi-line data is joined with `\n`.
    pub fn data(&self) -> Option<&str> {
        self.data.as_deref()
    }

    /// Returns the event ID, if set.
    ///
    /// Corresponds to the `id:` field in the SSE stream.
    /// Used with `Last-Event-ID` header for reconnection.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the retry duration, if set.
    ///
    /// Corresponds to the `retry:` field in the SSE stream.
    /// Indicates how long to wait before reconnecting after a dropped connection.
    pub fn retry(&self) -> Option<Duration> {
        self.retry
    }

    /// Returns the comment lines, if any.
    ///
    /// Corresponds to lines beginning with `:` in the SSE stream.
    /// Commonly used for keep-alive messages from the server.
    pub fn comment(&self) -> Option<&[String]> {
        self.comment.as_deref()
    }
}

/// A reader for SSE response body.
///
/// Wraps a streaming response body and parses it into [`SseEvent`]s.
pub struct SseReader<B> {
    body: B,
    /// Raw byte buffer accumulating bytes across frames
    buffer: Vec<u8>,
    /// Tracks the last received event ID for reconnection
    last_event_id: Option<String>,
    /// Whether this is the very first read (for UTF-8 BOM trimming)
    is_first_read: bool,
}

impl<B> SseReader<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    /// Create a new [`SseReader`] from an [`http::Response`], validating the
    /// `Content-Type` header is `text/event-stream`.
    pub fn new(resp: http::Response<B>) -> Result<Self, BoxError> {
        let content_type = resp
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !content_type.starts_with("text/event-stream") {
            return Err(ERR_INVALID_CONTENT_TYPE.into());
        }

        Ok(Self {
            body: resp.into_body(),
            buffer: Vec::new(),
            last_event_id: None,
            is_first_read: true,
        })
    }

    /// Returns the last event ID received, useful for reconnection via
    /// the `Last-Event-ID` request header.
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    /// Read the next SSE event from the stream.
    ///
    /// Returns:
    /// - `Ok(Some(event))` when an event is successfully parsed
    /// - `Ok(None)` when the stream has ended
    /// - `Err(e)` on IO or parse errors
    pub async fn read(&mut self) -> Result<Option<SseEvent>, BoxError> {
        let mut pending = SseEvent::default();
        let mut has_field = false;

        loop {
            // Process all complete lines already in the buffer
            while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
                let mut line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();

                // Remove trailing '\n'
                line_bytes.pop();

                // Remove trailing '\r' for \r\n line endings
                if line_bytes.last() == Some(&b'\r') {
                    line_bytes.pop();
                }

                let line = std::str::from_utf8(&line_bytes)?.to_string();

                // Trim UTF-8 BOM on very first line
                let line = if self.is_first_read {
                    self.is_first_read = false;
                    line.trim_start_matches('\u{FEFF}').to_string()
                } else {
                    line
                };

                if line.is_empty() {
                    // Blank line = end of event
                    if has_field {
                        if let Some(id) = &pending.id {
                            if !id.contains('\0') {
                                self.last_event_id = Some(id.clone());
                            }
                        }
                        return Ok(Some(pending));
                    }
                    continue;
                }

                // Comment line
                if line.starts_with(':') {
                    let comment_text = line[1..].trim_start().to_string();
                    pending
                        .comment
                        .get_or_insert_with(Vec::new)
                        .push(comment_text);
                    has_field = true; // comments count as fields
                    continue;
                }

                // Parse `field: value` or `field` (no colon = empty value)
                let (field, value) = match line.find(':') {
                    Some(idx) => {
                        let value = line[idx + 1..]
                            .strip_prefix(' ')
                            .unwrap_or(&line[idx + 1..])
                            .to_string();
                        (line[..idx].to_string(), value)
                    }
                    None => (line.clone(), String::new()),
                };

                has_field = true;

                match field.as_str() {
                    EVENT => pending.event = Some(value),
                    DATA => match &mut pending.data {
                        Some(existing) => {
                            existing.push('\n');
                            existing.push_str(&value);
                        }
                        None => pending.data = Some(value),
                    },
                    ID => pending.id = Some(value),
                    RETRY => {
                        if let Ok(ms) = value.parse::<u64>() {
                            pending.retry = Some(Duration::from_millis(ms));
                        }
                    }
                    _ => {} // Unknown fields ignored per spec
                }
            }

            // Pull next frame from body
            match Pin::new(&mut self.body).frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.buffer.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => return Err(e.into()),
                None => {
                    // Stream ended — dispatch any trailing event
                    if has_field {
                        if let Some(id) = &pending.id {
                            if !id.contains('\0') {
                                self.last_event_id = Some(id.clone());
                            }
                        }
                        return Ok(Some(pending));
                    }
                    return Ok(None);
                }
            }
        }
    }

    /// Iterate over all events, calling an **async closure** for each one.
    ///
    /// Stops when the stream ends or an error occurs.
    pub async fn for_each_async<F, Fut>(&mut self, mut f: F) -> Result<(), BoxError>
    where
        F: FnMut(SseEvent) -> Fut,
        Fut: std::future::Future<Output = Result<(), BoxError>>,
    {
        while let Some(event) = self.read().await? {
            f(event).await?;
        }
        Ok(())
    }
}
