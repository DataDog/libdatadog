// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    api::SampleType,
    internal::{Sample, Timestamp},
};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct Observations {
    sample_types: Box<[SampleType]>,
    aggregated: HashMap<SampleType, HashMap<Sample, i64>>,
    timestamped: HashMap<SampleType, HashMap<Sample, Vec<(i64, Timestamp)>>>,
}

impl Observations {
    pub fn new(sample_types: Box<[SampleType]>) -> Self {
        let len = sample_types.len();
        Self {
            sample_types,
            aggregated: HashMap::with_capacity(len),
            timestamped: HashMap::with_capacity(len),
        }
    }

    pub fn add(
        &mut self,
        sample: Sample,
        timestamp: Option<Timestamp>,
        values: &[i64],
    ) -> anyhow::Result<()> {
        for (idx, v) in values.iter().enumerate() {
            if *v != 0 {
                let sample_type = self.sample_types[idx];
                if let Some(ts) = timestamp {
                    self.timestamped
                        .entry(sample_type)
                        .or_default()
                        .entry(sample)
                        .or_default()
                        .push((*v, ts));
                } else {
                    let val = self
                        .aggregated
                        .entry(sample_type)
                        .or_default()
                        .entry(sample)
                        .or_insert(0);
                    *val = val.saturating_add(*v);
                }
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.aggregated.is_empty() && self.timestamped.is_empty()
    }

    pub fn aggregated_samples_count(&self) -> usize {
        // TODO: this is the actual sample count, but doesn't reflect aggregated samples being
        // overlapping self.aggregated.iter().map(|(_, v)| v.len()).sum()
        let samples: HashSet<&Sample> = self
            .aggregated
            .iter()
            .flat_map(|(_, samples)| samples.iter().map(|(s, _)| s))
            .collect();
        samples.len()
    }

    pub fn timestamped_samples_count(&self) -> usize {
        self.timestamped
            .iter()
            .flat_map(|(_, v)| v.iter().map(|(_, v)| v.len()))
            .sum()
    }

    pub fn try_into_iter(
        self,
    ) -> std::io::Result<impl Iterator<Item = (Sample, Option<Timestamp>, Vec<i64>)>> {
        Ok(self.into_iter())
    }

    pub fn into_iter(self) -> impl Iterator<Item = (Sample, Option<Timestamp>, Vec<i64>)> {
        let index_map: HashMap<SampleType, usize> = self
            .sample_types
            .iter()
            .enumerate()
            .map(|(idx, typ)| (*typ, idx))
            .collect();

        let len: usize = self.sample_types.len();
        let index_map_ts = index_map.clone();
        let accum_iter = self
            .aggregated
            .into_iter()
            .flat_map(move |(sample_type, inner)| {
                #[allow(clippy::unwrap_used)]
                let index = *index_map.get(&sample_type).unwrap();
                inner.into_iter().map(move |(sample, value)| {
                    let mut vals = vec![0; len];
                    vals[index] = value;
                    (sample, None, vals)
                })
            });

        // let mut accum: HashMap<Sample, Vec<i64>> = HashMap::new();
        // for (sample_type, samples) in self.aggregated.into_iter() {
        //     let Some(idx) = index_map.get(&sample_type) else {
        //         continue;
        //     };
        //     for (sample, val) in samples.into_iter() {
        //         let val_accum = accum.entry(sample).or_insert_with(|| vec![0; len]);
        //         val_accum[*idx] += val;
        //     }
        // }
        // let accum_iter = accum.into_iter().map(|(k, v)| (k, None, v));

        let ts_iter = self
            .timestamped
            .into_iter()
            .flat_map(move |(sample_type, inner)| {
                #[allow(clippy::unwrap_used)]
                let index = *index_map_ts.get(&sample_type).unwrap();
                inner.into_iter().flat_map(move |(sample, ts_vals)| {
                    ts_vals.into_iter().map(move |(value, ts)| {
                        let mut vals = vec![0; len];
                        vals[index] = value;
                        (sample, Some(ts), vals)
                    })
                })
            });
        accum_iter.chain(ts_iter)
    }
}
