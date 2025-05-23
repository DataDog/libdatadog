// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    encode_len_delimited, prost_impls, varint_len, Label, LenEncodable, TagEncodable, WireType,
};
use std::io::{self, Write};

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
        crate::key_len(tag, WireType::LengthDelimited)
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

    let encoded_len = items.iter().copied().map(varint_len).sum::<usize>();
    crate::encode_len_delimited_prefix(writer, tag, encoded_len as u64)?;
    for item in items {
        crate::varint(writer, *item)?;
    }
    Ok(())
}

#[inline]
fn packed_i64<W: Write>(writer: &mut W, tag: u32, items: &[i64]) -> io::Result<()> {
    // SAFETY: the pointer comes from a reference, and does a valid conversion.
    let items: &[u64] = unsafe { &*(items as *const [i64] as *const [u64]) };
    packed_varint(writer, tag, items)
}

impl TagEncodable for Sample<'_> {
    fn encode_with_tag<W: Write>(&self, w: &mut W, tag: u32) -> io::Result<()> {
        encode_len_delimited(w, tag, self)
    }
}

impl LenEncodable for Sample<'_> {
    fn encoded_len(&self) -> usize {
        let locations = packed_varint_u64_len(1, self.location_ids);
        let values = packed_varint_i64_len(2, self.values);
        let labels = self
            .labels
            .iter()
            .map(|label| crate::encoded_len(3, label).1)
            .sum::<usize>();
        locations + values + labels
    }

    fn encode_raw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        packed_varint(writer, 1, self.location_ids)?;
        packed_i64(writer, 2, self.values)?;
        for label in self.labels {
            encode_len_delimited(writer, 3, label)?;
        }
        Ok(())
    }
}

#[cfg(feature = "prost_impls")]
impl From<Sample<'_>> for prost_impls::Sample {
    fn from(sample: Sample) -> Self {
        Self {
            location_ids: Vec::from_iter(sample.location_ids.iter().copied()),
            values: Vec::from_iter(sample.values.iter().copied()),
            labels: sample.labels.iter().map(prost_impls::Label::from).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bolero::generator::TypeGenerator;
    use prost::Message;

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
        let mut buffer = Vec::with_capacity(len);
        sample.encode_raw(&mut buffer).unwrap();
        let roundtrip = prost_impls::Sample::decode(buffer.as_slice()).unwrap();
        assert_eq!(prost_sample, roundtrip);
    }

    #[test]
    fn roundtrip() {
        let locations = Vec::<u64>::produce();
        let values = Vec::<i64>::produce();
        let labels = Vec::<Label>::produce();

        bolero::check!()
            .with_generator((locations, values, labels))
            .for_each(|(location_ids, values, labels)| {
                let sample = Sample {
                    location_ids,
                    values,
                    labels,
                };

                let prost_sample = prost_impls::Sample::from(sample);

                let mut buffer = Vec::with_capacity(sample.encoded_len());
                sample.encode_raw(&mut buffer).unwrap();
                let roundtrip = prost_impls::Sample::decode(buffer.as_slice()).unwrap();
                assert_eq!(prost_sample, roundtrip);

                let mut buffer2 = Vec::with_capacity(sample.encoded_len());
                prost_sample.encode(&mut buffer2).unwrap();
                let roundtrip2 = prost_impls::Sample::decode(buffer2.as_slice()).unwrap();
                assert_eq!(roundtrip, roundtrip2);
            });
    }
}
