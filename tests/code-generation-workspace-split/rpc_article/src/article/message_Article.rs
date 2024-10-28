#[derive(
    PartialOrd,
    Hash,
    Eq,
    Ord,
    Debug,
    Default,
    ::pilota::serde::Serialize,
    ::pilota::serde::Deserialize,
    Clone,
    PartialEq,
)]
pub struct Article {
    pub id: i64,

    pub title: ::pilota::FastStr,

    pub content: ::pilota::FastStr,

    pub author: ::common::author::Author,

    pub status: Status,

    pub images: ::std::vec::Vec<::common::article::image::Image>,

    pub common_data: ::common::common::CommonData,
}
impl ::pilota::thrift::Message for Article {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::std::result::Result<(), ::pilota::thrift::ThriftException> {
        #[allow(unused_imports)]
        use ::pilota::thrift::TOutputProtocolExt;
        let struct_ident = ::pilota::thrift::TStructIdentifier { name: "Article" };

        __protocol.write_struct_begin(&struct_ident)?;
        __protocol.write_i64_field(1, *&self.id)?;
        __protocol.write_faststr_field(2, (&self.title).clone())?;
        __protocol.write_faststr_field(3, (&self.content).clone())?;
        __protocol.write_struct_field(4, &self.author, ::pilota::thrift::TType::Struct)?;
        __protocol.write_i32_field(5, (&self.status).inner())?;
        __protocol.write_list_field(
            6,
            ::pilota::thrift::TType::Struct,
            &&self.images,
            |__protocol, val| {
                __protocol.write_struct(val)?;
                ::std::result::Result::Ok(())
            },
        )?;
        __protocol.write_struct_field(7, &self.common_data, ::pilota::thrift::TType::Struct)?;
        __protocol.write_field_stop()?;
        __protocol.write_struct_end()?;
        ::std::result::Result::Ok(())
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
    ) -> ::std::result::Result<Self, ::pilota::thrift::ThriftException> {
        #[allow(unused_imports)]
        use ::pilota::{thrift::TLengthProtocolExt, Buf};

        let mut var_1 = None;
        let mut var_2 = None;
        let mut var_3 = None;
        let mut var_4 = None;
        let mut var_5 = None;
        let mut var_6 = None;
        let mut var_7 = None;

        let mut __pilota_decoding_field_id = None;

        __protocol.read_struct_begin()?;
        if let ::std::result::Result::Err(mut err) = (|| {
            loop {
                let field_ident = __protocol.read_field_begin()?;
                if field_ident.field_type == ::pilota::thrift::TType::Stop {
                    __protocol.field_stop_len();
                    break;
                } else {
                    __protocol.field_begin_len(field_ident.field_type, field_ident.id);
                }
                __pilota_decoding_field_id = field_ident.id;
                match field_ident.id {
                    Some(1) if field_ident.field_type == ::pilota::thrift::TType::I64 => {
                        var_1 = Some(__protocol.read_i64()?);
                    }
                    Some(2) if field_ident.field_type == ::pilota::thrift::TType::Binary => {
                        var_2 = Some(__protocol.read_faststr()?);
                    }
                    Some(3) if field_ident.field_type == ::pilota::thrift::TType::Binary => {
                        var_3 = Some(__protocol.read_faststr()?);
                    }
                    Some(4) if field_ident.field_type == ::pilota::thrift::TType::Struct => {
                        var_4 = Some(::pilota::thrift::Message::decode(__protocol)?);
                    }
                    Some(5) if field_ident.field_type == ::pilota::thrift::TType::I32 => {
                        var_5 = Some(::pilota::thrift::Message::decode(__protocol)?);
                    }
                    Some(6) if field_ident.field_type == ::pilota::thrift::TType::List => {
                        var_6 = Some(unsafe {
                            let list_ident = __protocol.read_list_begin()?;
                            let mut val: Vec<::common::article::image::Image> =
                                Vec::with_capacity(list_ident.size);
                            for i in 0..list_ident.size {
                                val.as_mut_ptr()
                                    .offset(i as isize)
                                    .write(::pilota::thrift::Message::decode(__protocol)?);
                            }
                            val.set_len(list_ident.size);
                            __protocol.read_list_end()?;
                            val
                        });
                    }
                    Some(7) if field_ident.field_type == ::pilota::thrift::TType::Struct => {
                        var_7 = Some(::pilota::thrift::Message::decode(__protocol)?);
                    }
                    _ => {
                        __protocol.skip(field_ident.field_type)?;
                    }
                }

                __protocol.read_field_end()?;
                __protocol.field_end_len();
            }
            ::std::result::Result::Ok::<_, ::pilota::thrift::ThriftException>(())
        })() {
            if let Some(field_id) = __pilota_decoding_field_id {
                err.prepend_msg(&format!(
                    "decode struct `Article` field(#{}) failed, caused by: ",
                    field_id
                ));
            }
            return ::std::result::Result::Err(err);
        };
        __protocol.read_struct_end()?;

        let Some(var_1) = var_1 else {
            return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                "field id is required".to_string(),
            ));
        };
        let Some(var_2) = var_2 else {
            return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                "field title is required".to_string(),
            ));
        };
        let Some(var_3) = var_3 else {
            return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                "field content is required".to_string(),
            ));
        };
        let Some(var_4) = var_4 else {
            return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                "field author is required".to_string(),
            ));
        };
        let Some(var_5) = var_5 else {
            return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                "field status is required".to_string(),
            ));
        };
        let Some(var_6) = var_6 else {
            return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                "field images is required".to_string(),
            ));
        };
        let Some(var_7) = var_7 else {
            return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                "field common_data is required".to_string(),
            ));
        };

        let data = Self {
            id: var_1,
            title: var_2,
            content: var_3,
            author: var_4,
            status: var_5,
            images: var_6,
            common_data: var_7,
        };
        ::std::result::Result::Ok(data)
    }

    fn decode_async<'a, T: ::pilota::thrift::TAsyncInputProtocol>(
        __protocol: &'a mut T,
    ) -> ::std::pin::Pin<
        ::std::boxed::Box<
            dyn ::std::future::Future<
                    Output = ::std::result::Result<Self, ::pilota::thrift::ThriftException>,
                > + Send
                + 'a,
        >,
    > {
        ::std::boxed::Box::pin(async move {
            let mut var_1 = None;
            let mut var_2 = None;
            let mut var_3 = None;
            let mut var_4 = None;
            let mut var_5 = None;
            let mut var_6 = None;
            let mut var_7 = None;

            let mut __pilota_decoding_field_id = None;

            __protocol.read_struct_begin().await?;
            if let ::std::result::Result::Err(mut err) = async {
                    loop {


                let field_ident = __protocol.read_field_begin().await?;
                if field_ident.field_type == ::pilota::thrift::TType::Stop {

                    break;
                } else {

                }
                __pilota_decoding_field_id = field_ident.id;
                match field_ident.id {
                    Some(1) if field_ident.field_type == ::pilota::thrift::TType::I64  => {
                    var_1 = Some(__protocol.read_i64().await?);

                },Some(2) if field_ident.field_type == ::pilota::thrift::TType::Binary  => {
                    var_2 = Some(__protocol.read_faststr().await?);

                },Some(3) if field_ident.field_type == ::pilota::thrift::TType::Binary  => {
                    var_3 = Some(__protocol.read_faststr().await?);

                },Some(4) if field_ident.field_type == ::pilota::thrift::TType::Struct  => {
                    var_4 = Some(<::common::author::Author as ::pilota::thrift::Message>::decode_async(__protocol).await?);

                },Some(5) if field_ident.field_type == ::pilota::thrift::TType::I32  => {
                    var_5 = Some(<Status as ::pilota::thrift::Message>::decode_async(__protocol).await?);

                },Some(6) if field_ident.field_type == ::pilota::thrift::TType::List  => {
                    var_6 = Some({
                            let list_ident = __protocol.read_list_begin().await?;
                            let mut val = Vec::with_capacity(list_ident.size);
                            for _ in 0..list_ident.size {
                                val.push(<::common::article::image::Image as ::pilota::thrift::Message>::decode_async(__protocol).await?);
                            };
                            __protocol.read_list_end().await?;
                            val
                        });

                },Some(7) if field_ident.field_type == ::pilota::thrift::TType::Struct  => {
                    var_7 = Some(<::common::common::CommonData as ::pilota::thrift::Message>::decode_async(__protocol).await?);

                },
                    _ => {
                        __protocol.skip(field_ident.field_type).await?;

                    },
                }

                __protocol.read_field_end().await?;


            };
                    ::std::result::Result::Ok::<_, ::pilota::thrift::ThriftException>(())
                }.await {
                if let Some(field_id) = __pilota_decoding_field_id {
                    err.prepend_msg(&format!("decode struct `Article` field(#{}) failed, caused by: ", field_id));
                }
                return ::std::result::Result::Err(err);
            };
            __protocol.read_struct_end().await?;

            let Some(var_1) = var_1 else {
                return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                    ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                    "field id is required".to_string(),
                ));
            };
            let Some(var_2) = var_2 else {
                return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                    ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                    "field title is required".to_string(),
                ));
            };
            let Some(var_3) = var_3 else {
                return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                    ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                    "field content is required".to_string(),
                ));
            };
            let Some(var_4) = var_4 else {
                return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                    ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                    "field author is required".to_string(),
                ));
            };
            let Some(var_5) = var_5 else {
                return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                    ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                    "field status is required".to_string(),
                ));
            };
            let Some(var_6) = var_6 else {
                return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                    ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                    "field images is required".to_string(),
                ));
            };
            let Some(var_7) = var_7 else {
                return ::std::result::Result::Err(::pilota::thrift::new_protocol_exception(
                    ::pilota::thrift::ProtocolExceptionKind::InvalidData,
                    "field common_data is required".to_string(),
                ));
            };

            let data = Self {
                id: var_1,
                title: var_2,
                content: var_3,
                author: var_4,
                status: var_5,
                images: var_6,
                common_data: var_7,
            };
            ::std::result::Result::Ok(data)
        })
    }

    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {
        #[allow(unused_imports)]
        use ::pilota::thrift::TLengthProtocolExt;
        __protocol.struct_begin_len(&::pilota::thrift::TStructIdentifier { name: "Article" })
            + __protocol.i64_field_len(Some(1), *&self.id)
            + __protocol.faststr_field_len(Some(2), &self.title)
            + __protocol.faststr_field_len(Some(3), &self.content)
            + __protocol.struct_field_len(Some(4), &self.author)
            + __protocol.i32_field_len(Some(5), (&self.status).inner())
            + __protocol.list_field_len(
                Some(6),
                ::pilota::thrift::TType::Struct,
                &self.images,
                |__protocol, el| __protocol.struct_len(el),
            )
            + __protocol.struct_field_len(Some(7), &self.common_data)
            + __protocol.field_stop_len()
            + __protocol.struct_end_len()
    }
}
