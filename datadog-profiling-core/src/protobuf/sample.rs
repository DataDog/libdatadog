// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::protobuf::encode::{varint_len, WireType, MAX_TAG, MAX_VARINT_LEN};
use crate::protobuf::{self, encode, Label, LenEncodable};
use datadog_alloc::buffer::FixedCapacityBuffer;
use std::io::{self, Write};
use std::mem;

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
        encode::key_len(tag, WireType::LengthDelimited)
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

fn packed_varint<W: Write>(writer: &mut W, tag: u32, items: &[u64]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    const STORAGE_LEN: usize =
        encode::key_len(MAX_TAG, WireType::LengthDelimited) + varint_len(u64::MAX);
    let mut storage: [mem::MaybeUninit<u8>; STORAGE_LEN] =
        unsafe { mem::transmute(mem::MaybeUninit::<[u8; STORAGE_LEN]>::uninit()) };
    let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());

    let encoded_len = items.iter().copied().map(varint_len).sum::<usize>();
    unsafe {
        encode::key(&mut buf, tag, WireType::LengthDelimited);
        encode::varint(&mut buf, encoded_len as u64);
    }
    writer.write_all(buf.as_slice())?;

    let mut storage: [mem::MaybeUninit<u8>; MAX_VARINT_LEN] =
        unsafe { mem::transmute(mem::MaybeUninit::<[u8; MAX_VARINT_LEN]>::uninit()) };
    for item in items {
        let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());
        unsafe { encode::varint(&mut buf, *item) };
        writer.write_all(buf.as_slice())?;
    }
    Ok(())
}

#[inline]
fn packed_i64<W: Write>(writer: &mut W, tag: u32, items: &[i64]) -> io::Result<()> {
    // SAFETY: the pointer comes from a reference, and does a valid conversion.
    let items: &[u64] = unsafe { &*(items as *const [i64] as *const [u64]) };
    packed_varint(writer, tag, items)
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

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        packed_varint(writer, 1, self.location_ids)?;
        packed_i64(writer, 2, self.values)?;
        for label in self.labels {
            protobuf::encode_len_delimited(writer, 3, label, label.encoded_len())?;
        }
        Ok(())
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
        let len = sample.encoded_len();
        let mut storage = Vec::with_capacity(len);
        let mut buffer = FixedCapacityBuffer::from(storage.spare_capacity_mut());
        sample.encode_raw(&mut buffer).unwrap();
        let roundtrip = prost_impls::Sample::decode(buffer.as_slice()).unwrap();
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
            // string Label
            Label {
                key: StringOffset { offset: 31 },
                str: StringOffset { offset: 32 },
                num: 0,
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
        let len = sample.encoded_len();
        let mut storage = Vec::with_capacity(len);
        let mut buffer = FixedCapacityBuffer::from(storage.spare_capacity_mut());
        sample.encode_raw(&mut buffer).unwrap();
        let roundtrip = prost_impls::Sample::decode(buffer.as_slice()).unwrap();
        assert_eq!(prost_sample, roundtrip);
    }
}
