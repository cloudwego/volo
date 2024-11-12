#[derive(Debug, Clone)]
pub enum AuthorServiceResponseRecv {
    GetAuthor(AuthorServiceGetAuthorResultRecv),
}

#[derive(Debug, Clone)]
pub enum AuthorServiceResponseSend {
    GetAuthor(AuthorServiceGetAuthorResultSend),
}

impl ::volo_thrift::EntryMessage for AuthorServiceResponseRecv {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetAuthor(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(::pilota::thrift::Message::decode(__protocol)?),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(
                <AuthorServiceGetAuthorResultRecv as ::pilota::thrift::Message>::decode_async(
                    __protocol,
                )
                .await?,
            ),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {
        match self {
            Self::GetAuthor(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}

impl ::volo_thrift::EntryMessage for AuthorServiceResponseSend {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetAuthor(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(::pilota::thrift::Message::decode(__protocol)?),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(
                <AuthorServiceGetAuthorResultSend as ::pilota::thrift::Message>::decode_async(
                    __protocol,
                )
                .await?,
            ),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {
        match self {
            Self::GetAuthor(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}
