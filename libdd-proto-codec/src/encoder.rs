use crate::constants::WireType;
use alloc::vec::Vec;
use core::ops::DerefMut;

pub const MAP_KEY_FIELD_NUM: u32 = 1;
pub const MAP_VALUE_FIELD_NUM: u32 = 2;

pub trait BufMut: DerefMut<Target = [u8]> {
    fn put_u8(&mut self, v: u8);
    fn put_slice(&mut self, slice: &[u8]);
    fn truncate(&mut self, new_len: usize);
}

impl BufMut for Vec<u8> {
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }

    fn put_slice(&mut self, slice: &[u8]) {
        self.extend_from_slice(slice);
    }

    fn truncate(&mut self, new_len: usize) {
        self.truncate(new_len);
    }
}

#[derive(Default)]
pub struct TopLevelEncoder<B: BufMut> {
    data: B,
}

impl<B: BufMut> TopLevelEncoder<B> {
    pub fn encoder(&mut self) -> Encoder<'_, B> {
        Encoder {
            data: &mut self.data,
        }
    }

    pub fn finish(self) -> B {
        self.data
    }
}

pub struct NestedEncoder<'a, B: BufMut> {
    tag_position: usize,
    size_position: usize,
    write_empty: bool,
    encoder: Encoder<'a, B>,
}

impl<B: BufMut> NestedEncoder<'_, B> {
    pub fn encoder(&mut self) -> Encoder<'_, B> {
        Encoder {
            data: self.encoder.data,
        }
    }
}

impl<B: BufMut> Drop for NestedEncoder<'_, B> {
    fn drop(&mut self) {
        let size = self.encoder.data.len() - self.size_position - 5;
        if !self.write_empty && size == 0 {
            // If the message is empty, we need to remove the tag and size
            self.encoder.data.truncate(self.tag_position);
            return;
        }

        let size_placeholder: &mut [u8; 5] = (&mut self.encoder.data
            [self.size_position..self.size_position + 5])
            .try_into()
            .unwrap();
        write_varint_max(size as u64, size_placeholder);
    }
}

trait ScalarEncoder {
    type Input;
    const WIRE_TYPE: WireType;

    fn encode<B: BufMut>(input: Self::Input, data: &mut B);
}

#[inline(always)]
const fn append_varint<T, B: BufMut, F: FnOnce(T) -> u64>(f: F) -> impl FnOnce(T, &mut B) {
    move |input: T, data: &mut B| {
        let v = f(input);
        encode_varint(v, data)
    }
}

macro_rules! impl_scalar_encode {
    ($ty:ident, $input_ty:ty, $write_fn:expr, $wire_ty:expr) => {
        struct $ty;
        impl ScalarEncoder for $ty {
            type Input = $input_ty;
            const WIRE_TYPE: WireType = $wire_ty;

            #[inline(always)]
            fn encode<B: BufMut>(input: Self::Input, data: &mut B) {
                $write_fn(input, data);
            }
        }
    };
}

macro_rules! impl_scalar_encode_varint {
    ($ty:ident, $input_ty:ty, $to_varint_fn:expr) => {
        impl_scalar_encode!(
            $ty,
            $input_ty,
            append_varint($to_varint_fn),
            WireType::Varint
        );
    };
}

impl_scalar_encode_varint!(UInt64Encoder, u64, |v| v);
impl_scalar_encode_varint!(UInt32Encoder, u32, |v| v as u64);
impl_scalar_encode_varint!(Int64Encoder, i64, |v| v as u64);
impl_scalar_encode_varint!(Int32Encoder, i32, |v| v as u64);
impl_scalar_encode_varint!(SInt64Encoder, i64, |v| ((v << 1) ^ (v >> 63)) as u64);
impl_scalar_encode_varint!(SInt32Encoder, i32, |v| ((v << 1) ^ (v >> 31)) as u32 as u64);
impl_scalar_encode_varint!(BoolEncoder, bool, |v| v as u64);
impl_scalar_encode!(
    Fixed64Encoder,
    u64,
    |v: u64, data: &mut B| {
        data.put_slice(&v.to_le_bytes());
    },
    WireType::Fixed64
);
impl_scalar_encode!(
    Fixed32Encoder,
    u32,
    |v: u32, data: &mut B| {
        data.put_slice(&v.to_le_bytes());
    },
    WireType::Fixed32
);
impl_scalar_encode!(
    SFixed64Encoder,
    i64,
    |v: i64, data: &mut B| {
        data.put_slice(&v.to_le_bytes());
    },
    WireType::Fixed64
);
impl_scalar_encode!(
    SFixed32Encoder,
    i32,
    |v: i32, data: &mut B| {
        data.put_slice(&v.to_le_bytes());
    },
    WireType::Fixed32
);
impl_scalar_encode!(
    F64Encoder,
    f64,
    |v: f64, data: &mut B| {
        let bits = v.to_bits();
        data.put_slice(&bits.to_le_bytes());
    },
    WireType::Fixed64
);
impl_scalar_encode!(
    F32Encoder,
    f32,
    |v: f32, data: &mut B| {
        let bits = v.to_bits();
        data.put_slice(&bits.to_le_bytes());
    },
    WireType::Fixed32
);

struct StringEncoder<'a>(core::marker::PhantomData<&'a ()>);

impl<'a> ScalarEncoder for StringEncoder<'a> {
    type Input = &'a str;
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    #[inline(always)]
    fn encode<B: BufMut>(input: Self::Input, data: &mut B) {
        BytesEncoder::encode(input.as_bytes(), data);
    }
}

struct BytesEncoder<'a>(core::marker::PhantomData<&'a ()>);

impl<'a> ScalarEncoder for BytesEncoder<'a> {
    type Input = &'a [u8];
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    #[inline(always)]
    fn encode<B: BufMut>(input: Self::Input, data: &mut B) {
        let size = input.len();
        encode_varint(size as u64, data);
        data.put_slice(input);
    }
}

fn encode_packed<E: ScalarEncoder, I: Iterator<Item = E::Input>, B: BufMut>(
    values: I,
    data: &mut B,
) {
    let size_position = data.len();
    data.put_slice(&[0; 5]); // Placeholder for size
    for value in values {
        E::encode(value, data);
    }
    let size = data.len() - size_position - 5;
    let size_placeholder: &mut [u8; 5] = (&mut data[size_position..size_position + 5])
        .try_into()
        .unwrap();
    write_varint_max(size as u64, size_placeholder);
}

#[derive(Debug)]
pub struct Encoder<'a, B: BufMut> {
    data: &'a mut B,
}

impl<B: BufMut> Encoder<'_, B> {
    /// returns an Encoder for a nested message.
    ///
    /// ```rust
    /// use libdd_proto_codec::encoder::{TopLevelEncoder, Encoder, BufMut};
    ///
    /// struct Bar {
    ///    baz: i32,
    /// }
    ///
    /// fn encode_bar<B: BufMut>(e: &mut Encoder<'_, B>, bar: &Bar) {
    ///     e.write_sint32(1, bar.baz);
    /// }
    ///
    /// struct Foo {
    ///     bar: Bar,
    /// }
    ///
    /// fn encode_foo<B: BufMut>(e: &mut Encoder<'_, B>, foo: &Foo) {
    ///     encode_bar(&mut e.write_message(1).encoder(), &foo.bar);
    /// }
    ///
    /// let mut e = TopLevelEncoder::<Vec<u8>>::default();
    /// encode_foo(&mut e.encoder(), &Foo { bar: Bar { baz: -1 } } );
    /// dbg!(e.finish());
    /// ```
    pub fn write_message(&mut self, field_number: u32) -> NestedEncoder<'_, B> {
        let tag_position = self.data.len();
        encode_tagged(field_number, WireType::LengthDelimited, self.data);
        let size_position = self.data.len();
        self.data.put_slice(&[0; 5]); // Placeholder for size
        NestedEncoder {
            tag_position,
            write_empty: false,
            size_position,
            encoder: Encoder { data: self.data },
        }
    }

    /// returns an Encoder for a nested message.
    ///
    /// If the nested message has a zero value (all fields are default or missing)
    /// it will still be encoded into the buffer
    pub fn write_message_opt(&mut self, field_number: u32) -> NestedEncoder<'_, B> {
        encode_tagged(field_number, WireType::LengthDelimited, self.data);
        let size_position = self.data.len();
        self.data.put_slice(&[0; 5]); // Placeholder for size
        NestedEncoder {
            // not needed
            tag_position: 0,
            write_empty: true,
            size_position,
            encoder: Encoder { data: self.data },
        }
    }

    /// returns an Encoder for a nested message with repeated annotation
    ///
    /// ```rust
    /// use libdd_proto_codec::encoder::{TopLevelEncoder, Encoder, BufMut};
    ///
    /// struct Bar {
    ///    baz: i32,
    /// }
    ///
    /// fn encode_bar<B: BufMut>(e: &mut Encoder<'_, B>, bar: &Bar) {
    ///     e.write_sint32(1, bar.baz);
    /// }
    ///
    /// struct Foo {
    ///     bars: Vec<Bar>,
    /// }
    ///
    /// fn encode_foo<B: BufMut>(e: &mut Encoder<'_, B>, foo: &Foo) {
    ///     for bar in &foo.bars {
    ///         encode_bar(&mut e.write_message(1).encoder(), &bar);
    ///    }
    /// }
    ///
    /// let mut e = TopLevelEncoder::<Vec<u8>>::default();
    /// encode_foo(&mut e.encoder(), &Foo { bars: vec![Bar { baz: -1 }, Bar { baz: 0 }] } );
    /// dbg!(e.finish());
    /// ```
    pub fn write_message_repeated(&mut self, field_number: u32) -> NestedEncoder<'_, B> {
        self.write_message_opt(field_number)
    }

    pub fn write_strings_repeated<'b, I: IntoIterator<Item = &'b str>>(
        &mut self,
        field_number: u32,
        v: I,
    ) {
        for value in v {
            self.write_string_repeated(field_number, value);
        }
    }

    pub fn write_bytess_repeated<'b, I: IntoIterator<Item = &'b [u8]>>(
        &mut self,
        field_number: u32,
        v: I,
    ) {
        for value in v {
            self.write_bytes_repeated(field_number, value);
        }
    }

    /// returns a helper to encode protobufs maps
    ///
    /// ```
    /// use libdd_proto_codec::encoder::{Encoder, MapEncoder, BufMut, MAP_KEY_FIELD_NUM, MAP_VALUE_FIELD_NUM};
    ///
    /// // message Example {
    /// //   map<string, int> field = 3;
    /// //}
    ///
    /// struct Example {
    ///     field: Vec<(String, i64)>,
    /// }
    /// fn encode_example(e: &mut Encoder<'_, Vec<u8>>, example: &Example) {
    ///     let map_encoder = e.write_map(3);
    /// }
    ///
    /// fn encode_string_i64_map<'a, I: IntoIterator<Item = &'a (String, i64)>>(mut e: MapEncoder<'_, Vec<u8>>, map: I) {
    ///     for (k, v) in map {
    ///         encode_string_i64_map_entry(&mut e.write_map_entry()
    ///             .encoder(), k, *v);
    ///     }
    /// }
    ///
    /// fn encode_string_i64_map_entry<B: BufMut>(e: &mut Encoder<'_, B>, key: &str, value: i64) {
    ///     e.write_string(MAP_KEY_FIELD_NUM, key);
    ///     e.write_int64(MAP_VALUE_FIELD_NUM, value);
    /// }
    /// ```
    pub fn write_map(&mut self, field_number: u32) -> MapEncoder<'_, B> {
        MapEncoder {
            data: self.data,
            field_number,
        }
    }
}

pub struct MapEncoder<'a, B: BufMut> {
    data: &'a mut B,
    field_number: u32,
}

impl<B: BufMut> MapEncoder<'_, B> {
    pub fn write_map_entry(&mut self) -> NestedEncoder<'_, B> {
        let tag_position = self.data.len();
        encode_tagged(self.field_number, WireType::LengthDelimited, self.data);
        let size_position = self.data.len();
        self.data.put_slice(&[0; 5]); // Placeholder for size
        NestedEncoder {
            tag_position,
            write_empty: true,
            size_position,
            encoder: Encoder { data: self.data },
        }
    }
}

macro_rules! impl_scalar {
    ($($fn_name:ident, $opt_fn_name:ident, $repeated_fn_name:ident, $($repeated_iter_fn_name:ident)?, $input_ty:ty, $encoder:ty,)*) => {
        impl <B: BufMut>Encoder<'_, B> {
            $(
                pub fn $fn_name(&mut self, field_number: u32, v: $input_ty) {
                    if v == <$input_ty> :: default() {
                        return;
                    }
                    encode_tagged(field_number, <$encoder>::WIRE_TYPE, self.data);
                    <$encoder>::encode(v, self.data);
                }

                pub fn $opt_fn_name(&mut self, field_number: u32, v: $input_ty) {
                    encode_tagged(field_number, <$encoder>::WIRE_TYPE, self.data);
                    <$encoder>::encode(v, self.data);
                }

                pub fn $repeated_fn_name(&mut self, field_number: u32, v: $input_ty) {
                    self.$opt_fn_name(field_number, v);
                }

                $(
                    pub fn $repeated_iter_fn_name<I: IntoIterator<Item = $input_ty>>(&mut self, field_number: u32, v: I) {
                        for value in v {
                            self.$repeated_fn_name(field_number, value);
                        }
                    }
                )?
            )*
        }
    };
}

macro_rules! impl_packed {
    ($($fn_name:ident, $input_ty:ty, $encoder:ty,)*) => {
        impl <B: BufMut>Encoder<'_, B> {
            $(
                pub fn $fn_name<I: IntoIterator<Item = $input_ty>>(&mut self, field_number: u32, v: I) {
                    let mut v = v.into_iter().peekable();
                    if v.peek().is_none() {
                        return;
                    }
                    encode_tagged(field_number, WireType::LengthDelimited, self.data);
                    encode_packed::<$encoder, _, B>(v, self.data);
                }
            )*
        }
    };
}

impl_scalar! {
    write_uint64, write_uint64_opt, write_uint64_repeated, write_uint64s_repeated, u64, UInt64Encoder,
    write_uint32, write_uint32_opt, write_uint32_repeated, write_uint32s_repeated, u32, UInt32Encoder,
    write_int64, write_int64_opt, write_int64_repeated, write_int64s_repeated, i64, Int64Encoder,
    write_int32, write_int32_opt, write_int32_repeated, write_int32s_repeated, i32, Int32Encoder,
    write_sint64, write_sint64_opt, write_sint64_repeated, write_sint64s_repeated, i64, SInt64Encoder,
    write_sint32, write_sint32_opt, write_sint32_repeated, write_sint32s_repeated, i32, SInt32Encoder,
    write_fixed64, write_fixed64_opt, write_fixed64_repeated, write_fixed64s_repeated, u64, Fixed64Encoder,
    write_fixed32, write_fixed32_opt, write_fixed32_repeated, write_fixed32s_repeated, u32, Fixed32Encoder,
    write_sfixed64, write_sfixed64_opt, write_sfixed64_repeated, write_sfixed64s_repeated, i64, SFixed64Encoder,
    write_sfixed32, write_sfixed32_opt, write_sfixed32_repeated, write_sfixed32s_repeated, i32, SFixed32Encoder,
    write_bool, write_bool_opt, write_bool_repeated, write_bools_repeated, bool, BoolEncoder,
    write_f64, write_f64_opt, write_f64_repeated, write_f64s_repeated, f64, F64Encoder,
    write_f32, write_f32_opt, write_f32_repeated, write_f32s_repeated, f32, F32Encoder,
    write_string, write_string_opt, write_string_repeated, , &str, StringEncoder<'_>,
    write_bytes, write_bytes_opt, write_bytes_repeated, , &[u8], BytesEncoder<'_>,
}

impl_packed! {
    write_uint64_packed, u64, UInt64Encoder,
    write_uint32_packed, u32, UInt32Encoder,
    write_int64_packed, i64, Int64Encoder,
    write_int32_packed, i32, Int32Encoder,
    write_sint64_packed, i64, SInt64Encoder,
    write_sint32_packed, i32, SInt32Encoder,
    write_fixed64_packed, u64, Fixed64Encoder,
    write_fixed32_packed, u32, Fixed32Encoder,
    write_sfixed64_packed, i64, SFixed64Encoder,
    write_sfixed32_packed, i32, SFixed32Encoder,
    write_bool_packed, bool, BoolEncoder,
    write_f64_packed, f64, F64Encoder,
    write_f32_packed, f32, F32Encoder,
}

#[test]
fn test_encoding() {
    let mut data = vec![];
    let mut encoder = Encoder { data: &mut data };

    encoder.write_message(1).encoder().write_uint32(1, 2);
    encoder.write_uint32(2, 3);
    assert_eq!(data, &[10, 130, 128, 128, 128, 0, 8, 2, 16, 3])
}

#[inline]
fn encode_tagged<B: BufMut>(field_number: u32, wire_type: WireType, data: &mut B) {
    let tag = (field_number << 3) | wire_type.to_u32();
    encode_varint(tag as u64, data);
}

#[inline]
fn write_varint_max(mut v: u64, buf: &mut [u8; 5]) {
    for (i, item) in buf.iter_mut().enumerate() {
        *item = (v & 0x7F) as u8;
        v >>= 7;
        if i != 4 {
            *item |= 0x80;
        }
    }
}

/// Encodes an integer value into LEB128 variable length format, and writes it to the buffer.
/// The buffer must have enough remaining space (maximum 10 bytes).
#[inline]
fn encode_varint<B: BufMut>(mut value: u64, buf: &mut B) {
    // Varints are never more than 10 bytes
    for _ in 0..10 {
        if value < 0x80 {
            buf.put_u8(value as u8);
            break;
        } else {
            buf.put_u8(((value & 0x7F) | 0x80) as u8);
            value >>= 7;
        }
    }
}
