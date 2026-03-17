// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    api::SampleType,
    internal::{Sample, Timestamp},
};
use std::collections::HashMap;

#[derive(Default)]
pub struct Observations {
    sample_types: Box<[SampleType]>,
    pub aggregated: HashMap<SampleType, HashMap<Sample, i64>>,
    pub aggregated2: HashMap<(SampleType, SampleType), HashMap<Sample, (i64, i64)>>,
    pub timestamped: HashMap<SampleType, HashMap<Sample, Vec<(i64, Timestamp)>>>,
    pub timestamped2:
        HashMap<(SampleType, SampleType), HashMap<Sample, Vec<(i64, i64, Timestamp)>>>,
}

impl Observations {
    pub fn new(sample_types: Box<[SampleType]>) -> Self {
        Self {
            sample_types,
            ..Default::default()
        }
    }

    pub fn add(
        &mut self,
        sample: Sample,
        timestamp: Option<Timestamp>,
        values: &[i64],
    ) -> anyhow::Result<()> {
        let mut first = None;
        let mut second = None;

        for (idx, v) in values.iter().enumerate() {
            if *v != 0 {
                let sample_type = self.sample_types[idx];
                if first.is_none() {
                    first = Some((sample_type, *v));
                } else if second.is_none() {
                    second = Some((sample_type, *v));
                } else {
                    anyhow::bail!("too many values");
                }
            }
        }

        match (first, second) {
            (None, None) => {}
            (Some((st, val)), None) => {
                if let Some(ts) = timestamp {
                    self.timestamped
                        .entry(st)
                        .or_default()
                        .entry(sample)
                        .or_default()
                        .push((val, ts));
                } else {
                    let entry = self
                        .aggregated
                        .entry(st)
                        .or_default()
                        .entry(sample)
                        .or_insert(0);
                    *entry = entry.saturating_add(val);
                }
            }
            (Some((st1, val1)), Some((st2, val2))) => {
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
            (None, Some(_)) => unreachable!("second set implies first set"),
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
        let index_map: HashMap<SampleType, usize> = self
            .sample_types
            .iter()
            .enumerate()
            .map(|(idx, typ)| (*typ, idx))
            .collect();

        let len: usize = self.sample_types.len();
        let index_map_ts = index_map.clone();
        let index_map_ts2 = index_map.clone();
        let index_map_accum2 = index_map.clone();
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

        let accum2_iter = self
            .aggregated2
            .into_iter()
            .flat_map(move |((st1, st2), inner)| {
                #[allow(clippy::unwrap_used)]
                let index1 = *index_map_accum2.get(&st1).unwrap();
                #[allow(clippy::unwrap_used)]
                let index2 = *index_map_accum2.get(&st2).unwrap();
                inner.into_iter().map(move |(sample, (val1, val2))| {
                    let mut vals = vec![0; len];
                    vals[index1] = val1;
                    vals[index2] = val2;
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

        let ts2_iter = self
            .timestamped2
            .into_iter()
            .flat_map(move |((st1, st2), inner)| {
                #[allow(clippy::unwrap_used)]
                let index1 = *index_map_ts2.get(&st1).unwrap();
                #[allow(clippy::unwrap_used)]
                let index2 = *index_map_ts2.get(&st2).unwrap();
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
