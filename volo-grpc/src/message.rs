use bytes::Bytes;
use http_body::Frame;

use crate::{
    body::BoxBody,
    codec::{CodecLimits, compression::CompressionEncoding, decode::Kind},
};

/// The homogeneous payload format of a registered service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PayloadKind {
    #[default]
    Protobuf,
    Thrift,
}

impl PayloadKind {
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Protobuf => "application/grpc",
            Self::Thrift => "application/grpc+thrift",
        }
    }

    pub fn default_limits(self) -> CodecLimits {
        match self {
            Self::Protobuf => CodecLimits::legacy(),
            Self::Thrift => CodecLimits::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcMode {
    Unary,
    ClientStreaming,
    ServerStreaming,
    Bidirectional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadShape {
    ProtobufMessage,
    UnaryArgsResult,
    StreamingRawStruct,
}

#[derive(Clone, Copy, Debug)]
pub struct MethodDescriptor {
    pub name: &'static str,
    pub mode: RpcMode,
    pub payload: PayloadShape,
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceDescriptor {
    pub payload: PayloadKind,
    pub methods: &'static [MethodDescriptor],
}

/// Additive codec options; old third-party entry messages keep their methods.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CodecOptions {
    pub limits: CodecLimits,
    pub context: Option<crate::streaming::StreamContext>,
}

impl CodecOptions {
    pub fn new(limits: CodecLimits) -> Self {
        Self {
            limits,
            context: None,
        }
    }

    pub fn with_context(mut self, context: Option<crate::streaming::StreamContext>) -> Self {
        self.context = context;
        self
    }
}

pub trait SendEntryMessage {
    fn payload_kind(&self) -> PayloadKind {
        PayloadKind::Protobuf
    }

    fn into_body(
        self,
        compression_config: Option<CompressionEncoding>,
    ) -> crate::BoxStream<'static, Result<Frame<Bytes>, crate::Status>>;

    fn into_body_with_options(
        self,
        compression_config: Option<CompressionEncoding>,
        _options: CodecOptions,
    ) -> crate::BoxStream<'static, Result<Frame<Bytes>, crate::Status>>
    where
        Self: Sized,
    {
        self.into_body(compression_config)
    }
}

#[allow(clippy::result_large_err)]
pub trait RecvEntryMessage: Sized {
    fn payload_kind() -> PayloadKind {
        PayloadKind::Protobuf
    }

    fn from_body(
        method: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression_encoding: Option<CompressionEncoding>,
    ) -> Result<Self, crate::Status>;

    fn from_body_with_options(
        method: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression_encoding: Option<CompressionEncoding>,
        _options: CodecOptions,
    ) -> Result<Self, crate::Status> {
        Self::from_body(method, body, kind, compression_encoding)
    }
}
