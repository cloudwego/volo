//! Validate variable-length fields before the generated decoder allocates or
//! slices. Primitive Binary reads and all size calculations still use Pilota.
use bytes::{Buf, Bytes};
use pilota::{
    FastStr,
    thrift::{
        ProtocolExceptionKind, TFieldIdentifier, TInputProtocol, TLengthProtocol, TListIdentifier,
        TMapIdentifier, TMessageIdentifier, TSetIdentifier, TStructIdentifier, TType,
        ThriftException, binary::TBinaryProtocol, new_protocol_exception,
    },
};

pub(super) struct CheckedBinaryInput<'a> {
    inner: TBinaryProtocol<&'a mut Bytes>,
    depth: usize,
    max_depth: usize,
    max_container_len: usize,
    detach_small_values: bool,
}

impl<'a> CheckedBinaryInput<'a> {
    pub(super) fn new(bytes: &'a mut Bytes, max_depth: usize, max_container_len: usize) -> Self {
        let detach_small_values = bytes.len() > 64 * 1024;
        Self {
            inner: TBinaryProtocol::new(bytes, true),
            depth: 0,
            max_depth,
            max_container_len,
            detach_small_values,
        }
    }

    fn enter(&mut self) -> Result<(), ThriftException> {
        if self.depth >= self.max_depth {
            return Err(new_protocol_exception(
                ProtocolExceptionKind::DepthLimit,
                "Thrift nesting exceeds limit",
            ));
        }
        self.depth += 1;
        Ok(())
    }

    fn length(&mut self) -> Result<usize, ThriftException> {
        usize::try_from(self.inner.read_i32()?).map_err(|_| {
            new_protocol_exception(
                ProtocolExceptionKind::NegativeSize,
                "negative Thrift length",
            )
        })
    }

    fn element_type(&mut self) -> Result<TType, ThriftException> {
        TType::try_from(self.inner.read_byte()?).map_err(|_| {
            new_protocol_exception(
                ProtocolExceptionKind::InvalidData,
                "invalid Thrift container element type",
            )
        })
    }

    fn count(&mut self, minimum_item_size: usize) -> Result<usize, ThriftException> {
        let count = self.length()?;
        if count > self.max_container_len {
            return Err(new_protocol_exception(
                ProtocolExceptionKind::SizeLimit,
                "Thrift container exceeds element limit",
            ));
        }
        if count != 0
            && (minimum_item_size == 0 || count > self.inner.buf().remaining() / minimum_item_size)
        {
            return Err(new_protocol_exception(
                ProtocolExceptionKind::InvalidData,
                "Thrift container length exceeds remaining payload",
            ));
        }
        Ok(count)
    }
}

fn minimum_size(kind: TType) -> usize {
    match kind {
        TType::Bool | TType::I8 | TType::Struct => 1,
        TType::I16 => 2,
        TType::I32 | TType::Binary => 4,
        TType::I64 | TType::Double => 8,
        TType::List | TType::Set => 5,
        TType::Map => 6,
        TType::Uuid => 16,
        TType::Stop | TType::Void => 0,
    }
}

macro_rules! lengths {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {$(
        #[inline]
        fn $name(&mut self, $($arg: $ty),*) -> usize {
            self.inner.$name($($arg),*)
        }
    )*};
}

impl TLengthProtocol for CheckedBinaryInput<'_> {
    lengths! {
        message_begin_len(identifier: &TMessageIdentifier);
        message_end_len();
        struct_begin_len(identifier: &TStructIdentifier);
        struct_end_len();
        field_begin_len(field_type: TType, id: Option<i16>);
        field_end_len();
        field_stop_len();
        bool_len(value: bool);
        bytes_len(value: &[u8]);
        bytes_vec_len(value: &[u8]);
        byte_len(value: u8);
        uuid_len(value: [u8; 16]);
        i8_len(value: i8);
        i16_len(value: i16);
        i32_len(value: i32);
        i64_len(value: i64);
        double_len(value: f64);
        string_len(value: &str);
        faststr_len(value: &FastStr);
        list_begin_len(identifier: TListIdentifier);
        list_end_len();
        set_begin_len(identifier: TSetIdentifier);
        set_end_len();
        map_begin_len(identifier: TMapIdentifier);
        map_end_len();
        zero_copy_len();
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

macro_rules! reads {
    ($($name:ident() -> $ty:ty;)*) => {$(
        #[inline]
        fn $name(&mut self) -> Result<$ty, ThriftException> { self.inner.$name() }
    )*};
}

impl TInputProtocol for CheckedBinaryInput<'_> {
    type Buf = Bytes;

    reads! {
        read_field_begin() -> TFieldIdentifier;
        read_field_end() -> ();
        read_bool() -> bool;
        read_uuid() -> [u8; 16];
        read_i8() -> i8;
        read_i16() -> i16;
        read_i32() -> i32;
        read_i64() -> i64;
        read_double() -> f64;
        read_byte() -> u8;
    }

    fn read_message_begin(&mut self) -> Result<TMessageIdentifier, ThriftException> {
        Err(new_protocol_exception(
            ProtocolExceptionKind::InvalidData,
            "gRPC Thrift payload has no message envelope",
        ))
    }
    fn read_message_end(&mut self) -> Result<(), ThriftException> {
        Err(new_protocol_exception(
            ProtocolExceptionKind::InvalidData,
            "gRPC Thrift payload has no message envelope",
        ))
    }
    fn read_struct_begin(&mut self) -> Result<Option<TStructIdentifier>, ThriftException> {
        self.enter()?;
        self.inner.read_struct_begin()
    }
    fn read_struct_end(&mut self) -> Result<(), ThriftException> {
        self.depth = self.depth.saturating_sub(1);
        self.inner.read_struct_end()
    }
    fn read_bytes(&mut self) -> Result<Bytes, ThriftException> {
        let len = self.length()?;
        self.get_bytes(None, len)
    }
    fn read_bytes_vec(&mut self) -> Result<Vec<u8>, ThriftException> {
        self.read_bytes().map(|bytes| bytes.to_vec())
    }
    fn read_faststr(&mut self) -> Result<FastStr, ThriftException> {
        FastStr::from_bytes(self.read_bytes()?).map_err(|_| {
            new_protocol_exception(
                ProtocolExceptionKind::InvalidData,
                "invalid UTF-8 in Thrift string",
            )
        })
    }
    fn read_string(&mut self) -> Result<String, ThriftException> {
        self.read_faststr().map(|value| value.to_string())
    }
    fn read_list_begin(&mut self) -> Result<TListIdentifier, ThriftException> {
        self.enter()?;
        let kind = self.element_type()?;
        let count = self.count(minimum_size(kind))?;
        Ok(TListIdentifier::new(kind, count))
    }
    fn read_list_end(&mut self) -> Result<(), ThriftException> {
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }
    fn read_set_begin(&mut self) -> Result<TSetIdentifier, ThriftException> {
        self.enter()?;
        let kind = self.element_type()?;
        let count = self.count(minimum_size(kind))?;
        Ok(TSetIdentifier::new(kind, count))
    }
    fn read_set_end(&mut self) -> Result<(), ThriftException> {
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }
    fn read_map_begin(&mut self) -> Result<TMapIdentifier, ThriftException> {
        self.enter()?;
        let key = self.element_type()?;
        let value = self.element_type()?;
        let key_min = minimum_size(key);
        let val_min = minimum_size(value);
        let count = self.count(if key_min == 0 || val_min == 0 {
            0
        } else {
            key_min + val_min
        })?;
        Ok(TMapIdentifier::new(key, value, count))
    }
    fn read_map_end(&mut self) -> Result<(), ThriftException> {
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }
    fn get_bytes(&mut self, ptr: Option<*const u8>, len: usize) -> Result<Bytes, ThriftException> {
        if ptr.is_none() && len > self.inner.buf().remaining() {
            return Err(new_protocol_exception(
                ProtocolExceptionKind::InvalidData,
                "Thrift binary length exceeds remaining payload",
            ));
        }
        if ptr.is_none() && self.detach_small_values && len < 4 * 1024 {
            let buf = self.inner.buf();
            let value = Bytes::copy_from_slice(&buf[..len]);
            buf.advance(len);
            return Ok(value);
        }
        self.inner.get_bytes(ptr, len)
    }
    fn buf(&mut self) -> &mut Self::Buf {
        self.inner.buf()
    }
}
