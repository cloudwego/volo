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

pub use volo::streaming::RpcMode;

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
    /// Use the optional optimized Thrift encoder. Protobuf is unaffected.
    pub unsafe_codec: bool,
    pub context: Option<crate::streaming::StreamContext>,
}

impl CodecOptions {
    pub fn new(limits: CodecLimits) -> Self {
        Self {
            limits,
            unsafe_codec: false,
            context: None,
        }
    }

    pub fn with_unsafe_codec(mut self, enabled: bool) -> Self {
        self.unsafe_codec = enabled;
        self
    }

    pub fn with_context(mut self, context: Option<crate::streaming::StreamContext>) -> Self {
        self.context = context;
        self
    }
}

// Old generated entry enums only implement into_body/from_body. Scope the explicit
// options for those synchronous constructors; never inherit an unrelated RPC's context.
tokio::task_local! { static LEGACY_CODEC_OPTIONS: CodecOptions; }
pub(crate) fn legacy_codec_options() -> Option<CodecOptions> {
    LEGACY_CODEC_OPTIONS.try_with(Clone::clone).ok()
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
        options: CodecOptions,
    ) -> crate::BoxStream<'static, Result<Frame<Bytes>, crate::Status>>
    where
        Self: Sized,
    {
        LEGACY_CODEC_OPTIONS.sync_scope(options, || self.into_body(compression_config))
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
        options: CodecOptions,
    ) -> Result<Self, crate::Status> {
        LEGACY_CODEC_OPTIONS.sync_scope(options, || {
            Self::from_body(method, body, kind, compression_encoding)
        })
    }
}
