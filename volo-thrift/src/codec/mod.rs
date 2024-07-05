use std::future::Future;

use pilota::thrift::ThriftException;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{context::ThriftContext, EntryMessage, ThriftMessage};

pub mod default;

pub use default::DefaultMakeCodec;

/// [`Decoder`] reads from an [`AsyncRead`] and decodes the data into a [`ThriftMessage`].
///
/// Returning an Ok(None) indicates the EOF has been reached.
///
/// Note: [`Decoder`] should be designed to be ready for reuse.
pub trait Decoder: Send + 'static {
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> impl Future<Output = Result<Option<ThriftMessage<Msg>>, ThriftException>> + Send;
}

/// [`Encoder`] writes a [`ThriftMessage`] to an [`AsyncWrite`] and flushes the data.
///
/// Note: [`Encoder`] should be designed to be ready for reuse.
pub trait Encoder: Send + 'static {
    fn encode<Req: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: ThriftMessage<Req>,
    ) -> impl Future<Output = Result<(), ThriftException>> + Send;
}

/// [`MakeCodec`] receives an [`AsyncRead`] and an [`AsyncWrite`] and returns a
/// [`Decoder`] and an [`Encoder`].
///
/// The implementation of [`MakeCodec`] must make sure the [`Decoder`] and [`Encoder`]
/// matches.
///
/// Hint: If the [`Decoder`] supports protocol probing, it can store the information in cx
/// and the [`Encoder`] can use it.
///
/// The reason why we split the [`Decoder`] and [`Encoder`] is that we want to support multiplex.
pub trait MakeCodec<R, W>: Clone + Send + 'static
where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    type Encoder: Encoder;
    type Decoder: Decoder;

    fn make_codec(&self, reader: R, writer: W) -> (Self::Encoder, Self::Decoder);
}
