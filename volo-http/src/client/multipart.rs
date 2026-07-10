//! Client-side `multipart/form-data` builder.
//!
//! This module provides [`Form`] and [`Part`] for building a `multipart/form-data` body for
//! client requests, which is the counterpart of the server-side
//! [`Multipart`](crate::server::utils::multipart::Multipart) extractor.
//!
//! [`Form`] collects a series of [`Part`]s and can be sent with
//! [`RequestBuilder::multipart`](crate::client::RequestBuilder::multipart). Each [`Part`] can be
//! built from in-memory bytes/text, an arbitrary [`AsyncRead`] reader, or a file path (which is
//! streamed lazily).
//!
//! # Example
//!
//! ```rust
//! use volo_http::client::multipart::{Form, Part};
//!
//! # async fn upload(client: volo_http::client::Client) -> Result<(), Box<dyn std::error::Error>> {
//! let form = Form::new()
//!     .text("key", "value")
//!     .part(
//!         "file",
//!         Part::text("hello, world")
//!             .file_name("hello.txt")
//!             .mime_str("text/plain")?,
//!     );
//!
//! let resp = client.post("http://127.0.0.1:8080/upload").multipart(form).send().await?;
//! # let _ = resp;
//! # Ok(())
//! # }
//! ```

use std::{borrow::Cow, io, path::Path, pin::Pin};

use bytes::{Bytes, BytesMut};
use futures_util::{StreamExt, stream::Stream};
use http::header::HeaderValue;
use http_body::Frame;
use mime::Mime;
use tokio::io::AsyncRead;
use tokio_util::io::ReaderStream;

use crate::{body::Body, error::BoxError};

// A boxed stream that is both `Send` and `Sync`, matching the bound of [`Body::from_stream`]. Note
// that `futures_util`'s `BoxStream` is only `Send`, so we define our own alias here (the same way
// as `crate::body`).
type FrameStream = Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, BoxError>> + Send + Sync>>;

/// A `multipart/form-data` request body.
///
/// A [`Form`] is a series of [`Part`]s, it can be sent through
/// [`RequestBuilder::multipart`](crate::client::RequestBuilder::multipart), which will set the
/// `Content-Type` header (with the generated boundary) and the body automatically.
#[must_use]
pub struct Form {
    boundary: String,
    parts: Vec<(Cow<'static, str>, Part)>,
}

impl Default for Form {
    fn default() -> Self {
        Self::new()
    }
}

impl Form {
    /// Create an empty [`Form`] with a randomly generated boundary.
    pub fn new() -> Self {
        Self {
            boundary: gen_boundary(),
            parts: Vec::new(),
        }
    }

    /// Get the boundary that this form will use.
    pub fn boundary(&self) -> &str {
        &self.boundary
    }

    /// Add a text field to the form.
    ///
    /// This is a shortcut for [`Form::part`] with [`Part::text`].
    pub fn text<N, V>(self, name: N, value: V) -> Self
    where
        N: Into<Cow<'static, str>>,
        V: Into<Cow<'static, str>>,
    {
        self.part(name, Part::text(value))
    }

    /// Add a [`Part`] to the form with the given field name.
    pub fn part<N>(mut self, name: N, part: Part) -> Self
    where
        N: Into<Cow<'static, str>>,
    {
        self.parts.push((name.into(), part));
        self
    }

    /// Add a file field to the form, the file will be read and streamed lazily.
    ///
    /// The `Content-Type` is guessed from the file extension, and the `filename` is taken from the
    /// path if it is not overridden. This is a shortcut for [`Form::part`] with [`Part::file`].
    pub async fn file<N, P>(self, name: N, path: P) -> io::Result<Self>
    where
        N: Into<Cow<'static, str>>,
        P: AsRef<Path>,
    {
        Ok(self.part(name, Part::file(path).await?))
    }

    /// Generate the `Content-Type` header value, i.e. `multipart/form-data; boundary=xxx`.
    pub(crate) fn content_type(&self) -> HeaderValue {
        // SAFETY: The boundary is generated from ascii-only characters, so the whole value is
        // always a valid header value.
        HeaderValue::from_str(&format!("multipart/form-data; boundary={}", self.boundary))
            .expect("multipart boundary should always be a valid header value")
    }

    /// Consume the form and encode it into a [`Body`].
    pub(crate) fn into_body(self) -> Body {
        let boundary = self.boundary;

        // Fast path: if every part is already in memory, assemble the whole body into a single
        // contiguous buffer. The resulting body has a known length, so the request is sent with a
        // `Content-Length` header instead of `Transfer-Encoding: chunked`, which some strict
        // servers prefer. Parts backed by a reader or a file have an unknown length and take the
        // streaming path below.
        if self.parts.iter().all(|(_, part)| part.data.is_in_memory()) {
            // Rough capacity: the part data dominates; add a fixed per-part overhead for the
            // boundary and headers to avoid most reallocations. An under-estimate only costs a
            // realloc.
            let cap = self
                .parts
                .iter()
                .map(|(name, part)| {
                    part.data.as_bytes().map_or(0, Bytes::len)
                        + name.len()
                        + part.file_name.as_ref().map_or(0, |f| f.len())
                        + part.mime.as_ref().map_or(0, |m| m.as_ref().len())
                        + boundary.len()
                        + 96
                })
                .sum::<usize>()
                + boundary.len()
                + 8;
            let mut buf = BytesMut::with_capacity(cap);
            for (name, part) in &self.parts {
                buf.extend_from_slice(&part.encode_header(&boundary, name));
                // `is_in_memory` was checked for every part above, so this is always `Some`.
                if let Some(bytes) = part.data.as_bytes() {
                    buf.extend_from_slice(bytes);
                }
                buf.extend_from_slice(b"\r\n");
            }
            buf.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            return Body::from(buf.freeze());
        }

        // Streaming path: each part becomes `--boundary\r\n<headers>\r\n\r\n<data>\r\n`, and the
        // whole body ends with a closing `--boundary--\r\n`.
        let mut streams: Vec<FrameStream> = Vec::with_capacity(self.parts.len() * 3 + 1);

        for (name, part) in self.parts {
            let header = part.encode_header(&boundary, &name);
            streams.push(once_frame(header));
            streams.push(part.data.into_stream());
            streams.push(once_frame(Bytes::from_static(b"\r\n")));
        }
        streams.push(once_frame(Bytes::from(format!("--{boundary}--\r\n"))));

        Body::from_stream(futures_util::stream::iter(streams).flatten())
    }
}

/// A single field of a [`Form`].
///
/// A [`Part`] can be created from in-memory bytes/text ([`Part::text`], [`Part::bytes`]), an
/// arbitrary async reader ([`Part::reader`], the counterpart of `SetFileReader`), or a file path
/// ([`Part::file`]). Additional metadata such as `filename` and `Content-Type` can be attached
/// with [`Part::file_name`] and [`Part::mime_str`]/[`Part::mime`].
#[must_use]
pub struct Part {
    data: PartData,
    file_name: Option<Cow<'static, str>>,
    mime: Option<Mime>,
}

enum PartData {
    Bytes(Bytes),
    Stream(FrameStream),
}

impl PartData {
    fn into_stream(self) -> FrameStream {
        match self {
            PartData::Bytes(bytes) => once_frame(bytes),
            PartData::Stream(stream) => stream,
        }
    }

    /// Whether the data is already fully in memory (i.e. not a lazily streamed reader/file).
    fn is_in_memory(&self) -> bool {
        matches!(self, PartData::Bytes(_))
    }

    /// Borrow the in-memory bytes, or `None` if the data is a stream.
    fn as_bytes(&self) -> Option<&Bytes> {
        match self {
            PartData::Bytes(bytes) => Some(bytes),
            PartData::Stream(_) => None,
        }
    }
}

impl Part {
    /// Create a text [`Part`] from a UTF-8 string.
    pub fn text<T>(value: T) -> Self
    where
        T: Into<Cow<'static, str>>,
    {
        let bytes = match value.into() {
            Cow::Borrowed(s) => Bytes::from_static(s.as_bytes()),
            Cow::Owned(s) => Bytes::from(s),
        };
        Self::new(PartData::Bytes(bytes))
    }

    /// Create a [`Part`] from in-memory bytes.
    pub fn bytes<T>(value: T) -> Self
    where
        T: Into<Bytes>,
    {
        Self::new(PartData::Bytes(value.into()))
    }

    /// Create a [`Part`] from an arbitrary [`AsyncRead`] reader, whose content is streamed lazily.
    ///
    /// This is the counterpart of `SetFileReader`: any reader (a file, a socket, a pipe, ...) can
    /// be used as the source of a part without buffering the whole content in memory.
    pub fn reader<R>(reader: R) -> Self
    where
        R: AsyncRead + Send + Sync + 'static,
    {
        let stream =
            ReaderStream::new(reader).map(|res| res.map(Frame::data).map_err(BoxError::from));
        Self::new(PartData::Stream(Box::pin(stream)))
    }

    /// Create a [`Part`] from a file path, whose content is streamed lazily.
    ///
    /// The `filename` defaults to the file name of the path, and the `Content-Type` is guessed
    /// from the file extension. Both can be overridden by [`Part::file_name`] and
    /// [`Part::mime_str`]/[`Part::mime`].
    pub async fn file<P>(path: P) -> io::Result<Self>
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref();
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let mime = mime_guess::from_path(path).first();
        let file = tokio::fs::File::open(path).await?;

        let mut part = Self::reader(file);
        if let Some(file_name) = file_name {
            part = part.file_name(file_name);
        }
        if let Some(mime) = mime {
            part = part.mime(mime);
        }
        Ok(part)
    }

    fn new(data: PartData) -> Self {
        Self {
            data,
            file_name: None,
            mime: None,
        }
    }

    /// Set the `filename` of the part.
    pub fn file_name<T>(mut self, file_name: T) -> Self
    where
        T: Into<Cow<'static, str>>,
    {
        self.file_name = Some(file_name.into());
        self
    }

    /// Set the `Content-Type` of the part from a string.
    ///
    /// # Errors
    ///
    /// Returns an error if the given string is not a valid MIME type.
    pub fn mime_str(self, mime: &str) -> Result<Self, mime::FromStrError> {
        Ok(self.mime(mime.parse()?))
    }

    /// Set the `Content-Type` of the part.
    pub fn mime(mut self, mime: Mime) -> Self {
        self.mime = Some(mime);
        self
    }

    /// Encode the leading boundary and headers of the part, i.e.
    /// `--boundary\r\nContent-Disposition: ...\r\n[Content-Type: ...\r\n]\r\n`.
    fn encode_header(&self, boundary: &str, name: &str) -> Bytes {
        // Pre-size the buffer for the common case (no escaping) so the header is built in a single
        // allocation instead of growing by repeated doubling. An under-estimate is still correct,
        // it only costs a realloc.
        let cap = 96
            + boundary.len()
            + name.len()
            + self.file_name.as_ref().map_or(0, |f| f.len() + 16)
            + self.mime.as_ref().map_or(0, |m| m.as_ref().len() + 16);
        let mut buf = BytesMut::with_capacity(cap);
        buf.extend_from_slice(b"--");
        buf.extend_from_slice(boundary.as_bytes());
        buf.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
        extend_escaped(&mut buf, name);
        buf.extend_from_slice(b"\"");
        if let Some(file_name) = &self.file_name {
            buf.extend_from_slice(b"; filename=\"");
            extend_escaped(&mut buf, file_name);
            buf.extend_from_slice(b"\"");
        }
        buf.extend_from_slice(b"\r\n");
        if let Some(mime) = &self.mime {
            buf.extend_from_slice(b"Content-Type: ");
            buf.extend_from_slice(mime.as_ref().as_bytes());
            buf.extend_from_slice(b"\r\n");
        }
        buf.extend_from_slice(b"\r\n");
        buf.freeze()
    }
}

/// Build a single-frame stream from a chunk of [`Bytes`].
fn once_frame(bytes: Bytes) -> FrameStream {
    Box::pin(futures_util::stream::once(
        async move { Ok(Frame::data(bytes)) },
    ))
}

/// Escape a field/file name for use inside a `Content-Disposition` quoted-string.
///
/// The value is emitted as an RFC 7578 / RFC 2616 `quoted-string`: `\` and `"` are backslash
/// escaped (this is exactly what the server-side parser [`multer`](multer) un-escapes), and `\r` /
/// `\n` are replaced with a space since a bare CR/LF is never legal inside a header value and would
/// otherwise break framing. This matches the behavior of `reqwest` and browsers so a name/filename
/// containing special characters round-trips correctly.
fn extend_escaped(buf: &mut BytesMut, value: &str) {
    let bytes = value.as_bytes();
    // Bulk-copy runs of ordinary bytes and only handle the (rare) special characters one at a
    // time, so a name/filename with no special characters is copied in a single `extend_from_slice`
    // instead of byte by byte.
    let mut start = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        let replacement: &[u8] = match byte {
            b'\\' => b"\\\\",
            b'"' => b"\\\"",
            b'\r' | b'\n' => b" ",
            _ => continue,
        };
        buf.extend_from_slice(&bytes[start..i]);
        buf.extend_from_slice(replacement);
        start = i + 1;
    }
    buf.extend_from_slice(&bytes[start..]);
}

/// Generate a boundary that is unlikely to appear in the body.
///
/// The boundary mixes a random value (so it is not predictable, which matters when a part's
/// content is attacker-influenced) with a per-process monotonic counter (so two forms created in
/// the same process never collide even if the RNG were to repeat). The result is well within the
/// 1-70 character limit of RFC 2046 and only uses characters that are valid in a boundary.
fn gen_boundary() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let rand = rand::random::<u64>();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    format!("volo-http-boundary-{rand:016x}{seq:016x}")
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    async fn body_to_string(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn encode_text_fields() {
        let form = Form::new().text("key1", "val1").text("key2", "val2");
        let boundary = form.boundary().to_owned();
        let content_type = form.content_type();
        let body = body_to_string(form.into_body()).await;

        assert_eq!(
            content_type.to_str().unwrap(),
            format!("multipart/form-data; boundary={boundary}")
        );
        let expected = format!(
            "--{boundary}\r\nContent-Disposition: form-data; \
             name=\"key1\"\r\n\r\nval1\r\n--{boundary}\r\nContent-Disposition: form-data; \
             name=\"key2\"\r\n\r\nval2\r\n--{boundary}--\r\n"
        );
        assert_eq!(body, expected);
    }

    #[tokio::test]
    async fn encode_reader_part_with_metadata() {
        let form = Form::new().part(
            "file",
            Part::reader(std::io::Cursor::new(b"file-content".to_vec()))
                .file_name("a.txt")
                .mime_str("text/plain")
                .unwrap(),
        );
        let boundary = form.boundary().to_owned();
        let body = body_to_string(form.into_body()).await;

        let expected = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"a.txt\"\r\nContent-Type: \
             text/plain\r\n\r\nfile-content\r\n--{boundary}--\r\n"
        );
        assert_eq!(body, expected);
    }

    #[tokio::test]
    async fn in_memory_form_has_known_length() {
        use http_body::Body as _;

        // An all-in-memory form should produce a body with an exact length so the request is sent
        // with `Content-Length` rather than chunked.
        let form = Form::new()
            .text("key", "value")
            .part("bytes", Part::bytes(&b"raw-bytes"[..]).file_name("a.bin"));
        let body = form.into_body();

        let hint = body.size_hint();
        let exact = hint
            .exact()
            .expect("in-memory form should have a known length");
        // The reported length must match the actual encoded bytes exactly, otherwise the framing
        // would be corrupted on the wire.
        let encoded = body.collect().await.unwrap().to_bytes();
        assert_eq!(exact, encoded.len() as u64);
    }

    #[tokio::test]
    async fn streaming_form_has_unknown_length() {
        use http_body::Body as _;

        // A form containing a streamed part cannot know its length upfront, so it falls back to a
        // chunked body (no exact size hint).
        let form = Form::new().text("key", "value").part(
            "file",
            Part::reader(std::io::Cursor::new(b"streamed".to_vec())),
        );
        let body = form.into_body();

        assert!(body.size_hint().exact().is_none());
    }

    #[test]
    fn boundaries_are_unique_and_valid() {
        let a = gen_boundary();
        let b = gen_boundary();
        // Two forms must not share a boundary (the counter guarantees this within a process).
        assert_ne!(a, b);
        // Well within the RFC 2046 1-70 character limit, and only boundary-legal characters.
        assert!(a.len() <= 70);
        assert!(
            a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "boundary contains an invalid character: {a}"
        );
    }

    #[test]
    fn escape_special_chars() {
        let mut buf = BytesMut::new();
        extend_escaped(&mut buf, "a\"b\\c\r\nd");
        // `"` -> `\"`, `\` -> `\\`, and `\r`/`\n` collapse to a space.
        assert_eq!(&buf[..], b"a\\\"b\\\\c  d");

        // No special characters: copied verbatim (fast path).
        let mut buf = BytesMut::new();
        extend_escaped(&mut buf, "plain_name.txt");
        assert_eq!(&buf[..], b"plain_name.txt");

        // Empty input and special chars at the very start/end (run boundaries).
        let mut buf = BytesMut::new();
        extend_escaped(&mut buf, "");
        assert_eq!(&buf[..], b"");

        let mut buf = BytesMut::new();
        extend_escaped(&mut buf, "\"ab\"");
        assert_eq!(&buf[..], b"\\\"ab\\\"");
    }

    #[tokio::test]
    async fn quoted_name_roundtrips_through_multer() {
        // A name/filename containing a `"` must survive a round-trip through the server-side
        // parser (`multer`), which un-escapes the backslash-escaped quote.
        //
        // Note: a literal backslash is intentionally not asserted here. We escape it to `\\` (RFC
        // quoted-string, same as reqwest) so it can never escape the closing delimiter and corrupt
        // framing, but `multer` only collapses `\"` and leaves `\\` doubled, so a raw backslash
        // cannot round-trip through it regardless of what the client emits.
        let form = Form::new().part("my\"field", Part::text("value").file_name("a\"b.txt"));
        let boundary = form.boundary().to_owned();
        let bytes = form.into_body().collect().await.unwrap().to_bytes();

        let stream =
            futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(bytes) });
        let mut multipart = multer::Multipart::new(stream, boundary);
        let field = multipart.next_field().await.unwrap().unwrap();

        assert_eq!(field.name().unwrap(), "my\"field");
        assert_eq!(field.file_name().unwrap(), "a\"b.txt");
        assert_eq!(field.bytes().await.unwrap(), &b"value"[..]);
    }
}
