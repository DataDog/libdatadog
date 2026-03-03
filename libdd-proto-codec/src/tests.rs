use arbitrary::Arbitrary;
use prost::Message;

use crate::encoder::{self, MAP_KEY_FIELD_NUM, MAP_VALUE_FIELD_NUM};

#[derive(PartialEq, prost::Message, arbitrary::Arbitrary)]
struct Bar {
    #[prost(message, tag = "1", required)]
    foo: Foo,
    #[prost(string, repeated, tag = "2")]
    string_repeated_field: Vec<String>,
    #[prost(sfixed32, repeated, tag = "3")]
    i32_repeated_field: Vec<i32>,
    #[prost(string, tag = "4")]
    string_field: String,
    #[prost(map = "string, sint64", tag = "5")]
    map_field: std::collections::HashMap<String, i64>,
}

#[derive(prost::Message, arbitrary::Arbitrary)]
struct Foo {
    #[prost(uint64, tag = "1")]
    u64_field: u64,
    #[prost(uint32, tag = "2")]
    u32_field: u32,
    #[prost(int64, tag = "3")]
    i64_field: i64,
    #[prost(int32, tag = "4")]
    i32_field: i32,
    #[prost(sint64, tag = "5")]
    si64_field: i64,
    #[prost(sint32, tag = "6")]
    si32_field: i32,
    #[prost(bool, tag = "7")]
    bool_field: bool,
    #[prost(double, tag = "8")]
    f64_field: f64,
    #[prost(float, tag = "9")]
    f32_field: f32,
    #[prost(sint64, repeated, packed, tag = "10")]
    packed_si64_packed_field: Vec<i64>,
    #[prost(string, tag = "11")]
    string_field: String,
}

impl PartialEq for Foo {
    fn eq(&self, other: &Self) -> bool {
        self.u64_field == other.u64_field
            && self.u32_field == other.u32_field
            && self.i64_field == other.i64_field
            && self.i32_field == other.i32_field
            && self.si64_field == other.si64_field
            && self.si32_field == other.si32_field
            && self.bool_field == other.bool_field
            && self.f64_field.total_cmp(&other.f64_field).is_eq()
            && self.f32_field.total_cmp(&other.f32_field).is_eq()
            && self.packed_si64_packed_field == other.packed_si64_packed_field
            && self.string_field == other.string_field
    }
}

fn manual_encode_bar<B: crate::encoder::BufMut>(e: &mut encoder::Encoder<'_, B>, bar: &Bar) {
    manual_encode_foo(&mut e.write_message(1).encoder(), &bar.foo);
    e.write_strings_repeated(2, bar.string_repeated_field.iter().map(|s| s.as_str()));
    e.write_sfixed32_packed(3, bar.i32_repeated_field.iter().copied());
    e.write_string(4, &bar.string_field);
    let mut map_enc = e.write_map(5);
    for (k, v) in &bar.map_field {
        let mut entry = map_enc.write_map_entry();
        let mut entry_enc = entry.encoder();
        entry_enc.write_string(MAP_KEY_FIELD_NUM, k);
        entry_enc.write_sint64(MAP_VALUE_FIELD_NUM, *v);
    }
}

fn manual_bar_top_level_encoder(bar: &Bar) -> Vec<u8> {
    let mut encoder = encoder::TopLevelEncoder::default();
    manual_encode_bar(&mut encoder.encoder(), bar);
    encoder.finish()
}

fn manual_encode_foo<B: crate::encoder::BufMut>(e: &mut encoder::Encoder<'_, B>, foo: &Foo) {
    e.write_uint64(1, foo.u64_field);
    e.write_uint32(2, foo.u32_field);
    e.write_int64(3, foo.i64_field);
    e.write_int32(4, foo.i32_field);
    e.write_sint64(5, foo.si64_field);
    e.write_sint32(6, foo.si32_field);
    e.write_bool(7, foo.bool_field);
    e.write_f64(8, foo.f64_field);
    e.write_f32(9, foo.f32_field);
    e.write_sint64_packed(10, foo.packed_si64_packed_field.iter().copied());
    e.write_string(11, &foo.string_field);
}

fn manual_foo_top_level_encoder(foo: &Foo) -> Vec<u8> {
    let mut encoder = encoder::TopLevelEncoder::default();
    manual_encode_foo(&mut encoder.encoder(), foo);
    encoder.finish()
}

#[test]
fn test_roundtrip_bar() {
    for _ in 0..100 {
        let l = rand::random_range(0..255_usize);
        let input: Vec<u8> = (0..l).map(|_| rand::random()).collect();
        let input_bar: Bar = Bar::arbitrary(&mut arbitrary::Unstructured::new(&input)).unwrap();

        test_roundtrip_bar_inner(&input_bar);
    }
}

fn test_roundtrip_bar_inner(input_bar: &Bar) {
    let manual_encoded = manual_bar_top_level_encoder(input_bar);
    let prost_encoded = input_bar.encode_to_vec();

    let prost_decoded_prost_encoded = Bar::decode(&*prost_encoded).unwrap();
    let prost_decoded_manual_encoded = Bar::decode(&*manual_encoded).unwrap();

    assert_eq!(&prost_decoded_prost_encoded, input_bar);
    assert_eq!(&prost_decoded_manual_encoded, input_bar);
}

#[test]
fn test_roundtrip_foo() {
    for _ in 0..100 {
        let l = rand::random_range(0..255_usize);
        let input: Vec<u8> = (0..l).map(|_| rand::random()).collect();
        let input_foo: Foo = Foo::arbitrary(&mut arbitrary::Unstructured::new(&input)).unwrap();
        test_roundtrip_foo_inner(&input_foo);
    }
}

fn test_roundtrip_foo_inner(input_foo: &Foo) {
    let manual_encoded = manual_foo_top_level_encoder(input_foo);
    let prost_encoded = input_foo.encode_to_vec();

    let prost_decoded_prost_encoded = Foo::decode(&*prost_encoded).unwrap();

    let prost_decoded_manual_encoded = Foo::decode(&*manual_encoded).unwrap();

    assert_eq!(&prost_decoded_prost_encoded, input_foo);
    assert_eq!(&prost_decoded_manual_encoded, input_foo);
}
