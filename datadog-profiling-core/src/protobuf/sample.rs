// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::protobuf::encode::varint_len;
use crate::protobuf::{self, encode, Buffer, ByteRange, Label, LenEncodable};
use datadog_alloc::buffer::MayGrowOps;

#[derive(Copy, Clone, Debug)]
pub struct Sample<'a> {
    pub location_ids: &'a [u64], // 1
    pub values: &'a [i64],       // 2
    pub labels: &'a [Label],     // 3
}

#[must_use]
#[inline]
fn packed_varint_u64_len(tag: u32, items: &[u64]) -> usize {
    if !items.is_empty() {
        let encoded_len = items.iter().copied().map(varint_len).sum::<usize>();
        encode::key_len(tag, encode::WireType::LengthDelimited)
            + varint_len(encoded_len as u64)
            + encoded_len
    } else {
        0
    }
}

#[must_use]
#[inline]
fn packed_varint_i64_len(tag: u32, items: &[i64]) -> usize {
    // SAFETY: the pointer comes from a reference, and does a valid conversion.
    let items: &[u64] = unsafe { &*(items as *const [i64] as *const [u64]) };
    packed_varint_u64_len(tag, items)
}

unsafe fn packed_varint<T: MayGrowOps<u8>>(buffer: &mut Buffer<T>, tag: u32, items: &[u64]) {
    if items.is_empty() {
        return;
    }
    unsafe {
        encode::key(buffer, tag, encode::WireType::LengthDelimited);
    }

    let encoded_len = items.iter().copied().map(varint_len).sum::<usize>();
    unsafe { encode::varint(buffer, encoded_len as u64) };
    for item in items {
        unsafe { encode::varint(buffer, *item) };
    }
}

#[inline]
unsafe fn packed_i64<T: MayGrowOps<u8>>(buffer: &mut Buffer<T>, tag: u32, items: &[i64]) {
    // SAFETY: the pointer comes from a reference, and does a valid conversion.
    let items: &[u64] = unsafe { &*(items as *const [i64] as *const [u64]) };
    unsafe { packed_varint(buffer, tag, items) };
}

impl LenEncodable for Sample<'_> {
    fn encoded_len(&self) -> usize {
        let locations = packed_varint_u64_len(1, self.location_ids);
        let values = packed_varint_i64_len(2, self.values);
        let labels = self
            .labels
            .iter()
            .map(|label| protobuf::encoded_len(3, label).1)
            .sum::<usize>();
        locations + values + labels
    }

    unsafe fn encode_raw<T: MayGrowOps<u8>>(&self, buffer: &mut Buffer<T>) -> ByteRange {
        let start = buffer.len_u31();
        unsafe { packed_varint(buffer, 1, self.location_ids) };
        unsafe { packed_i64(buffer, 2, self.values) };
        for label in self.labels {
            unsafe { protobuf::encode_len_delimited(buffer, 3, label, label.encoded_len()) };
        }
        let end = buffer.len_u31();
        ByteRange { start, end }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prost_impls;
    use crate::protobuf::StringOffset;
    use std::vec;

    #[test]
    fn empty() {
        let sample = Sample {
            location_ids: &[],
            values: &[],
            labels: &[],
        };
        let prost_sample = prost_impls::Sample {
            location_ids: vec![],
            values: vec![],
            labels: vec![],
        };

        use prost::Message;
        let mut storage = vec::Vec::new();
        let len = sample.encoded_len();
        storage.reserve(len);
        let mut buffer = Buffer::try_from(&mut storage).unwrap();
        unsafe { sample.encode_raw(&mut buffer) };
        let roundtrip = prost_impls::Sample::decode(&storage[..]).unwrap();
        assert_eq!(prost_sample, roundtrip);
    }

    #[test]
    fn roundtrip() {
        let location_ids = vec![u8::MAX as u64, u16::MAX as u64, u32::MAX as u64, u64::MAX];
        let values = vec![i8::MAX as i64, i16::MAX as i64, i32::MAX as i64, i64::MAX];
        let labels = [
            // similar to a local root span id
            Label {
                key: StringOffset { offset: 255 },
                str: StringOffset::ZERO,
                num: i64::MAX,
                num_unit: StringOffset::ZERO,
            },
            // similar to a pid
            Label {
                key: StringOffset { offset: 67 },
                str: StringOffset::ZERO,
                num: 0x7FFF,
                num_unit: StringOffset::ZERO,
            },
        ];
        let sample = Sample {
            location_ids: &location_ids,
            values: &values,
            labels: &labels,
        };

        let prost_sample = prost_impls::Sample {
            location_ids: location_ids.clone(),
            values: values.clone(),
            labels: labels
                .iter()
                .map(|label| prost_impls::Label {
                    key: label.key.offset as i64,
                    str: label.str.offset as i64,
                    num: label.num,
                    num_unit: label.num_unit.offset as i64,
                })
                .collect(),
        };

        use prost::Message;
        let mut storage = vec::Vec::new();
        let len = sample.encoded_len();
        storage.reserve(len);
        let mut buffer = Buffer::try_from(&mut storage).unwrap();
        unsafe { sample.encode_raw(&mut buffer) };
        let roundtrip = prost_impls::Sample::decode(&storage[..]).unwrap();
        assert_eq!(prost_sample, roundtrip);
    }
}
