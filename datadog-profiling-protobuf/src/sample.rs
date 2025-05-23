// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{prost_impls, Label, PackedVarint, Value, WireType};
use std::io::{self, Write};

#[derive(Copy, Clone, Debug)]
pub struct Sample<'a> {
    pub location_ids: &'a [u64], // 1
    pub values: &'a [i64],       // 2
    pub labels: &'a [Label],     // 3
}

impl Value for Sample<'_> {
    const WIRE_TYPE: WireType = WireType::LengthDelimited;

    fn proto_len(&self) -> u64 {
        let locations = PackedVarint::new(self.location_ids).field(1).proto_len();
        let values = PackedVarint::new(self.values).field(2).proto_len();
        let labels = self
            .labels
            .iter()
            .map(|label| label.field(3).proto_len())
            .sum::<u64>();
        locations + values + labels
    }

    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        PackedVarint::new(self.location_ids)
            .field(1)
            .encode(writer)?;
        PackedVarint::new(self.values).field(2).encode(writer)?;

        for label in self.labels {
            label.field(3).encode(writer)?;
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
            location_ids: Vec::from_iter(sample.location_ids.iter().copied()),
            values: Vec::from_iter(sample.values.iter().copied()),
            labels: sample.labels.iter().map(prost_impls::Label::from).collect(),
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
                let sample = Sample {
                    location_ids,
                    values,
                    labels,
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
