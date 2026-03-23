// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    api::SampleType,
    internal::{Sample, Timestamp},
};
use anyhow::Context;
use enum_map::EnumMap;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
#[allow(clippy::type_complexity)]
pub struct Observations {
    sample_types: Arc<[SampleType]>,
    sample_type_index_map: EnumMap<SampleType, Option<usize>>,
    pub paired_samples: Vec<Option<usize>>,
    pub aggregated: HashMap<SampleType, HashMap<Sample, i64>>,
    pub aggregated2: HashMap<(SampleType, SampleType), HashMap<Sample, (i64, i64)>>,
    pub timestamped: HashMap<SampleType, HashMap<Sample, Vec<(i64, Timestamp)>>>,
    pub timestamped2:
        HashMap<(SampleType, SampleType), HashMap<Sample, Vec<(i64, i64, Timestamp)>>>,
}

impl Observations {
    pub fn new(
        sample_types: Arc<[SampleType]>,
        sample_type_index_map: EnumMap<SampleType, Option<usize>>,
    ) -> Self {
        let len = sample_types.len();
        Self {
            sample_types,
            sample_type_index_map,
            paired_samples: vec![None; len],
            ..Default::default()
        }
    }

    pub fn pair_samples(&mut self, s1: SampleType, s2: SampleType) -> anyhow::Result<()> {
        let i1 = self.sample_type_index_map[s1].context("invalid sample type")?;
        let i2 = self.sample_type_index_map[s2].context("invalid sample type")?;
        anyhow::ensure!(
            self.paired_samples[i1].is_none() && self.paired_samples[i2].is_none(),
            "sample type already paired"
        );
        self.paired_samples[i1] = Some(i2);
        self.paired_samples[i2] = Some(i1);
        Ok(())
    }

    pub fn add(
        &mut self,
        sample: Sample,
        timestamp: Option<Timestamp>,
        values: &[i64],
    ) -> anyhow::Result<()> {
        for i in 0..values.len() {
            let val1 = values[i];

            if let Some(pair) = self.paired_samples[i] {
                let val2 = values[pair];
                let st1 = self.sample_types[i];
                let st2 = self.sample_types[pair];
                use enum_map::Enum as _;
                // Process each pair exactly once by requiring the canonical-first type
                // (lower enum index) to drive the insertion. This matches the ordering
                // used by OtelUpscalingRules, which stores pair rules under the lower
                // into_usize() type so both sides agree on which type is the map key.
                if st1.into_usize() < st2.into_usize() && (val1 != 0 || val2 != 0) {
                    if let Some(ts) = timestamp {
                        self.timestamped2
                            .entry((st1, st2))
                            .or_default()
                            .entry(sample)
                            .or_default()
                            .push((val1, val2, ts));
                    } else {
                        let entry = self
                            .aggregated2
                            .entry((st1, st2))
                            .or_default()
                            .entry(sample)
                            .or_insert((0, 0));
                        entry.0 = entry.0.saturating_add(val1);
                        entry.1 = entry.1.saturating_add(val2);
                    }
                }
            } else if val1 != 0 {
                let st = self.sample_types[i];
                if let Some(ts) = timestamp {
                    self.timestamped
                        .entry(st)
                        .or_default()
                        .entry(sample)
                        .or_default()
                        .push((val1, ts));
                } else {
                    let entry = self
                        .aggregated
                        .entry(st)
                        .or_default()
                        .entry(sample)
                        .or_insert(0);
                    *entry = entry.saturating_add(val1);
                }
            }
        }

        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.aggregated.is_empty() && self.timestamped.is_empty()
    }

    pub fn aggregated_samples_count(&self) -> usize {
        self.aggregated
            .values()
            .map(|samples| samples.len())
            .sum::<usize>()
            + self
                .aggregated2
                .values()
                .map(|samples| samples.len())
                .sum::<usize>()
    }

    pub fn timestamped_samples_count(&self) -> usize {
        self.timestamped
            .iter()
            .flat_map(|(_, v)| v.values().map(|v| v.len()))
            .sum::<usize>()
            + self
                .timestamped2
                .iter()
                .flat_map(|(_, v)| v.values().map(|v| v.len()))
                .sum::<usize>()
    }

    pub fn try_into_iter(
        self,
    ) -> std::io::Result<impl Iterator<Item = (Sample, Option<Timestamp>, Vec<i64>)>> {
        Ok(self.into_iter())
    }

    // TODO make this a trait impl
    #[allow(clippy::should_implement_trait)]
    pub fn into_iter(self) -> impl Iterator<Item = (Sample, Option<Timestamp>, Vec<i64>)> {
        let index_map = self.sample_type_index_map;

        // Invariant: all keys in aggregated/aggregated2/timestamped/timestamped2 come from
        // add(), which indexes into self.sample_types for non-zero values. So every
        // sample_type key is present in index_map.
        let len: usize = self.sample_types.len();
        let accum_iter = self
            .aggregated
            .into_iter()
            .flat_map(move |(sample_type, inner)| {
                #[allow(clippy::unwrap_used)]
                let index = index_map[sample_type].unwrap();
                inner.into_iter().map(move |(sample, value)| {
                    let mut vals = vec![0; len];
                    vals[index] = value;
                    (sample, None, vals)
                })
            });

        let accum2_iter = self
            .aggregated2
            .into_iter()
            .flat_map(move |((st1, st2), inner)| {
                #[allow(clippy::unwrap_used)]
                let index1 = index_map[st1].unwrap();
                #[allow(clippy::unwrap_used)]
                let index2 = index_map[st2].unwrap();
                inner.into_iter().map(move |(sample, (val1, val2))| {
                    let mut vals = vec![0; len];
                    vals[index1] = val1;
                    vals[index2] = val2;
                    (sample, None, vals)
                })
            });

        let ts_iter = self
            .timestamped
            .into_iter()
            .flat_map(move |(sample_type, inner)| {
                #[allow(clippy::unwrap_used)]
                let index = index_map[sample_type].unwrap();
                inner.into_iter().flat_map(move |(sample, ts_vals)| {
                    ts_vals.into_iter().map(move |(value, ts)| {
                        let mut vals = vec![0; len];
                        vals[index] = value;
                        (sample, Some(ts), vals)
                    })
                })
            });

        let ts2_iter = self
            .timestamped2
            .into_iter()
            .flat_map(move |((st1, st2), inner)| {
                #[allow(clippy::unwrap_used)]
                let index1 = index_map[st1].unwrap();
                #[allow(clippy::unwrap_used)]
                let index2 = index_map[st2].unwrap();
                inner.into_iter().flat_map(move |(sample, ts_vals)| {
                    ts_vals.into_iter().map(move |(val1, val2, ts)| {
                        let mut vals = vec![0; len];
                        vals[index1] = val1;
                        vals[index2] = val2;
                        (sample, Some(ts), vals)
                    })
                })
            });

        accum_iter.chain(accum2_iter).chain(ts_iter).chain(ts2_iter)
    }
}
