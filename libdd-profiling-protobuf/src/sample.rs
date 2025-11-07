// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{prost_impls, Label, Record, Value, WireType, NO_OPT_ZERO};
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
}
