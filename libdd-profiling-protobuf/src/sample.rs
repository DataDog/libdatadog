// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{Label, NO_OPT_ZERO, Record, Value, WireType, varint};

#[cfg(feature = "prost_impls")]
use crate::prost_impls;

use std::io::{self, Write};

/// Each Sample records values encountered in some program context. The
/// program context is typically a stack trace, perhaps augmented with
/// auxiliary information like the thread-id, some indicator of a higher level
/// request being handled, etc.
///
/// It borrows its data but requires it to be a slice. An iterator wouldn't
/// work well because we have to walk over the fields twice: one to calculate
/// the length, and one to encode it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Sample<'a> {
    /// The ids recorded here correspond to a Profile.location.id.
    /// The leaf is at location_id\[0\].
    pub location_ids: Record<&'a [u64], 1, NO_OPT_ZERO>,
    /// The type and unit of each value is defined by the corresponding entry
    /// in Profile.sample_type. All samples must have the same number of
    /// values, the same as the length of Profile.sample_type. When
    /// aggregating multiple samples into a single sample, the result has a
    /// list of values that is the element-wise sum of the original lists.
    pub values: Record<&'a [i64], 2, NO_OPT_ZERO>,
    /// NOTE: While possible, having multiple values for the same label key is
    /// strongly discouraged and should never be used. Most tools (e.g. pprof)
    /// do not have good (or any) support for multi-value labels. And an even
    /// more discouraged case is having a string label and a numeric label of
    /// the same name on a sample. Again, possible to express, but should not
    /// be used.
    pub labels: &'a [Record<Label, 3, NO_OPT_ZERO>],
}

/// # Safety
/// The Default implementation will return all zero-representations.
unsafe impl Value for Sample<'_> {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        self.location_ids.proto_len()
            + self.values.proto_len()
            + self.labels.iter().map(Record::proto_len).sum::<u64>()
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.location_ids.encode(writer)?;
        self.values.encode(writer)?;
        for label in self.labels {
            label.encode(writer)?;
        }
        Ok(())
    }
}

/// Encodes a sequence of `u64` values as packed protobuf varints into `out`.
///
/// Uses fallible capacity reservation so OOM returns an `io::ErrorKind::OutOfMemory` error.
pub fn encode_packed_u64<I: ExactSizeIterator<Item = u64>>(
    ids: I,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    out.clear();
    // Reserve at least 2 bytes per id (typical for location IDs) up front.
    let hint = ids.len().saturating_mul(2);
    out.try_reserve(hint)
        .map_err(|e| io::Error::new(io::ErrorKind::OutOfMemory, e))?;
    let mut buf = [0u8; 10];
    for id in ids {
        let len = varint::encode_to_slice(id, &mut buf);
        out.try_reserve(len)
            .map_err(|e| io::Error::new(io::ErrorKind::OutOfMemory, e))?;
        out.extend_from_slice(&buf[..len]);
    }
    Ok(())
}

/// Reusable single-pass encoder for `Profile.sample` (`Record<Sample, 2, NO_OPT_ZERO>`).
///
/// Encodes each sample payload once into an internal buffer and writes it in two
/// `write_all` calls (tag + length prefix, then payload), avoiding two-pass `proto_len`
/// traversals and byte-at-a-time writes.
#[derive(Default)]
pub struct SampleEncoder {
    sample_buf: Vec<u8>,
    values_buf: Vec<u8>,
}

impl SampleEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Encodes a `Sample` as field 2 (`NO_OPT_ZERO`) using pre-encoded packed `location_ids`.
    ///
    /// Produces byte-for-byte identical wire output to
    /// `Record::<Sample, 2, NO_OPT_ZERO>::encode`.
    pub fn encode_sample_record<W: Write>(
        &mut self,
        packed_location_ids: &[u8],
        values: &[i64],
        labels: impl Iterator<Item = Label>,
        writer: &mut W,
    ) -> io::Result<()> {
        const SAMPLE_TAG: u8 = ((2u32 << 3) | (WireType::LengthDelimited as u32)) as u8;
        const LOCATION_IDS_TAG: u8 = ((1u32 << 3) | (WireType::LengthDelimited as u32)) as u8;
        const VALUES_TAG: u8 = ((2u32 << 3) | (WireType::LengthDelimited as u32)) as u8;
        const LABEL_TAG: u8 = ((3u32 << 3) | (WireType::LengthDelimited as u32)) as u8;
        const LABEL_KEY_TAG: u8 = ((1u32 << 3) | (WireType::Varint as u32)) as u8;
        const LABEL_STR_TAG: u8 = ((2u32 << 3) | (WireType::Varint as u32)) as u8;
        const LABEL_NUM_TAG: u8 = ((3u32 << 3) | (WireType::Varint as u32)) as u8;
        const LABEL_NUM_UNIT_TAG: u8 = ((4u32 << 3) | (WireType::Varint as u32)) as u8;

        self.values_buf.clear();
        let values_hint = values.len().saturating_mul(10);
        self.values_buf
            .try_reserve(values_hint)
            .map_err(|e| io::Error::new(io::ErrorKind::OutOfMemory, e))?;
        for &v in values {
            varint::encode_to_vec(v as u64, &mut self.values_buf);
        }

        self.sample_buf.clear();
        let base_needed = 1 + 10 + packed_location_ids.len() + 1 + 10 + self.values_buf.len();
        self.sample_buf
            .try_reserve(base_needed)
            .map_err(|e| io::Error::new(io::ErrorKind::OutOfMemory, e))?;

        // Field 1: packed location_ids (NO_OPT_ZERO)
        self.sample_buf.push(LOCATION_IDS_TAG);
        varint::encode_to_vec(packed_location_ids.len() as u64, &mut self.sample_buf);
        self.sample_buf.extend_from_slice(packed_location_ids);

        // Field 2: packed values (NO_OPT_ZERO)
        self.sample_buf.push(VALUES_TAG);
        varint::encode_to_vec(self.values_buf.len() as u64, &mut self.sample_buf);
        self.sample_buf.extend_from_slice(&self.values_buf);

        // Field 3: repeated Label (NO_OPT_ZERO per entry, OPT_ZERO per Label field)
        // Max Label payload: 4 fields * (1 tag + 10 varint) = 44 bytes.
        let mut label_buf = [0u8; 48];
        let mut vbuf = [0u8; 10];
        for label in labels {
            let mut pos = 0usize;
            let key = u64::from(label.key.value);
            if key != 0 {
                label_buf[pos] = LABEL_KEY_TAG;
                pos += 1;
                let n = varint::encode_to_slice(key, &mut vbuf);
                label_buf[pos..pos + n].copy_from_slice(&vbuf[..n]);
                pos += n;
            }
            let str_val = u64::from(label.str.value);
            if str_val != 0 {
                label_buf[pos] = LABEL_STR_TAG;
                pos += 1;
                let n = varint::encode_to_slice(str_val, &mut vbuf);
                label_buf[pos..pos + n].copy_from_slice(&vbuf[..n]);
                pos += n;
            }
            let num = label.num.value;
            if num != 0 {
                label_buf[pos] = LABEL_NUM_TAG;
                pos += 1;
                let n = varint::encode_to_slice(num as u64, &mut vbuf);
                label_buf[pos..pos + n].copy_from_slice(&vbuf[..n]);
                pos += n;
            }
            let num_unit = u64::from(label.num_unit.value);
            if num_unit != 0 {
                label_buf[pos] = LABEL_NUM_UNIT_TAG;
                pos += 1;
                let n = varint::encode_to_slice(num_unit, &mut vbuf);
                label_buf[pos..pos + n].copy_from_slice(&vbuf[..n]);
                pos += n;
            }

            // 1 tag + up to 2 len bytes (pos <= 44 fits in 1 byte) + pos bytes
            self.sample_buf
                .try_reserve(1 + 10 + pos)
                .map_err(|e| io::Error::new(io::ErrorKind::OutOfMemory, e))?;
            self.sample_buf.push(LABEL_TAG);
            varint::encode_to_vec(pos as u64, &mut self.sample_buf);
            self.sample_buf.extend_from_slice(&label_buf[..pos]);
        }

        // Write outer Record<Sample, 2, NO_OPT_ZERO> header + payload.
        let mut hdr = [0u8; 11];
        hdr[0] = SAMPLE_TAG;
        let len_bytes = varint::encode_to_slice(self.sample_buf.len() as u64, &mut vbuf);
        hdr[1..1 + len_bytes].copy_from_slice(&vbuf[..len_bytes]);
        writer.write_all(&hdr[..1 + len_bytes])?;
        writer.write_all(&self.sample_buf)
    }
}

#[cfg(feature = "prost_impls")]
impl From<Sample<'_>> for prost_impls::Sample {
    fn from(sample: Sample) -> Self {
        // If the prost file is regenerated, this may pick up new members.
        #[allow(clippy::needless_update)]
        Self {
            location_ids: Vec::from_iter(sample.location_ids.value.iter().copied()),
            values: Vec::from_iter(sample.values.value.iter().copied()),
            labels: sample
                .labels
                .iter()
                .map(|field| field.value)
                .map(prost_impls::Label::from)
                .collect(),
            ..Self::default()
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
            location_ids: [].as_slice().into(),
            values: [].as_slice().into(),
            labels: &[],
        };
        let prost_sample = prost_impls::Sample {
            location_ids: vec![],
            values: vec![],
            labels: vec![],
        };

        use prost::Message;
        let len = sample.proto_len() as usize;
        let mut buffer = Vec::with_capacity(len);
        sample.encode(&mut buffer).unwrap();
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
                let labels = labels
                    .iter()
                    .map(|l| Record::<_, 3, NO_OPT_ZERO>::from(*l))
                    .collect::<Vec<_>>();
                let sample = Sample {
                    location_ids: Record::from(location_ids.as_slice()),
                    values: Record::from(values.as_slice()),
                    labels: labels.as_slice(),
                };

                let prost_sample = prost_impls::Sample::from(sample);

                let mut buffer = Vec::with_capacity(sample.proto_len() as usize);
                sample.encode(&mut buffer).unwrap();
                let roundtrip = prost_impls::Sample::decode(buffer.as_slice()).unwrap();
                assert_eq!(prost_sample, roundtrip);

                let mut buffer2 = Vec::with_capacity(sample.proto_len() as usize);
                prost_sample.encode(&mut buffer2).unwrap();
                let roundtrip2 = prost_impls::Sample::decode(buffer2.as_slice()).unwrap();
                assert_eq!(roundtrip, roundtrip2);
            });
    }

    #[test]
    fn sample_encoder_matches_record_encode_byte_for_byte() {
        let locations = Vec::<u64>::produce();
        let values = Vec::<i64>::produce();
        let labels = Vec::<Label>::produce();
        let mut encoder = SampleEncoder::new();
        let mut packed_locations = Vec::new();

        bolero::check!()
            .with_generator((locations, values, labels))
            .for_each(|(location_ids, values, labels)| {
                let record_labels = labels
                    .iter()
                    .map(|l| Record::<_, 3, NO_OPT_ZERO>::from(*l))
                    .collect::<Vec<_>>();
                let sample = Sample {
                    location_ids: Record::from(location_ids.as_slice()),
                    values: Record::from(values.as_slice()),
                    labels: record_labels.as_slice(),
                };

                let mut expected = Vec::new();
                Record::<_, 2, NO_OPT_ZERO>::from(sample)
                    .encode(&mut expected)
                    .unwrap();

                encode_packed_u64(location_ids.iter().copied(), &mut packed_locations).unwrap();
                let mut actual = Vec::new();
                encoder
                    .encode_sample_record(
                        &packed_locations,
                        values.as_slice(),
                        labels.iter().copied(),
                        &mut actual,
                    )
                    .unwrap();

                assert_eq!(expected, actual);
            });
    }
}
