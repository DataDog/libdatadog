// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! See the mod.rs file comment for why this module and file exists.

use super::super::Sample;
use super::timestamped_observations::TimestampedObservations;
use super::trimmed_observation::{ObservationLength, TrimmedObservation};
use crate::internal::Timestamp;
use std::collections::HashMap;

struct NonEmptyObservations {
    // Samples with no timestamps are aggregated in-place as each observation is added
    aggregated_data: HashMap<Sample, TrimmedObservation>,
    // Samples with timestamps are all separately kept (so we can know the exact values at the given timestamp)
    timestamped_data: Option<TimestampedObservations>,
    obs_len: ObservationLength,
    timestamped_samples_count: usize,
}

#[derive(Default)]
pub struct Observations {
    inner: Option<NonEmptyObservations>,
}

/// Public API
impl Observations {
    pub fn new(observations_len: usize) -> Self {
        Observations { inner: Some(Self::initialize(observations_len)) }
    }

    fn initialize(observations_len: usize) -> NonEmptyObservations {
        NonEmptyObservations {
            aggregated_data: Default::default(),
            timestamped_data: Some(TimestampedObservations::new(observations_len)),
            obs_len: ObservationLength::new(observations_len),
            timestamped_samples_count: 0,
        }
    }

    pub fn add(&mut self, sample: Sample, timestamp: Option<Timestamp>, values: Vec<i64>) {
        if let Some(inner) = &self.inner {
            inner.obs_len.assert_eq(values.len());
        } else {
            self.inner = Some(Self::initialize(values.len()));
        };

        // SAFETY: we just ensured it has an item above.
        let observations = unsafe { self.inner.as_mut().unwrap_unchecked() };
        let obs_len = observations.obs_len;

        if let Some(ts) = timestamp {
            observations
                .timestamped_data
                .as_mut()
                .unwrap()
                .add(sample, ts, values);
            observations.timestamped_samples_count += 1;
        } else if let Some(v) = observations.aggregated_data.get_mut(&sample) {
            // SAFETY: This method is only way to build one of these, and at
            // the top we already checked the length matches.
            unsafe { v.as_mut_slice(obs_len) }
                .iter_mut()
                .zip(values)
                .for_each(|(a, b)| *a += b);
        } else {
            let trimmed = TrimmedObservation::new(values, obs_len);
            observations.aggregated_data.insert(sample, trimmed);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_none() ||
            (self.aggregated_samples_count() == 0 && self.timestamped_samples_count() == 0)
    }

    pub fn aggregated_samples_count(&self) -> usize {
        self.inner
            .as_ref()
            .map(|o| o.aggregated_data.len())
            .unwrap_or(0)
    }

    pub fn timestamped_samples_count(&self) -> usize {
        self.inner
            .as_ref()
            .map(|o| o.timestamped_samples_count)
            .unwrap_or(0)
    }
}

pub struct ObservationsIntoIter {
    it: Box<dyn Iterator<Item = <ObservationsIntoIter as IntoIterator>::Item>>,
}

impl Iterator for ObservationsIntoIter {
    type Item = (Sample, Option<Timestamp>, Vec<i64>);
    fn next(&mut self) -> Option<Self::Item> {
        self.it.next()
    }
}

impl IntoIterator for Observations {
    type Item = (Sample, Option<Timestamp>, Vec<i64>);
    type IntoIter = ObservationsIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        let it = self.inner.into_iter().flat_map(|mut observations| {
            let timestamped_data_it = std::mem::take(&mut observations.timestamped_data)
                .unwrap()
                .into_iter()
                .map(|(s, t, o)| (s, Some(t), o));
            let aggregated_data_it = std::mem::take(&mut observations.aggregated_data)
                .into_iter()
                .map(|(s, o)| (s, None, o))
                .map(move |(s, t, o)| (s, t, unsafe { o.into_vec(observations.obs_len) }));
            timestamped_data_it.chain(aggregated_data_it)
        });
        ObservationsIntoIter { it: Box::new(it) }
    }
}

impl Drop for NonEmptyObservations {
    fn drop(&mut self) {
        let o = self.obs_len;
        self.aggregated_data.drain().for_each(|(_, v)| {
            // SAFETY: The only way to build one of these is through
            // [Self::add], which already checked that the length was correct.
            unsafe { v.consume(o) };
        });
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::collections::identifiable::*;
    use crate::internal::{LabelSetId, StackTraceId};
    use std::num::NonZeroI64;

    #[test]
    fn add_and_iter_test() {
        let mut o = Observations::default();
        // These are only for test purposes. The only thing that matters is that
        // they differ
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };
        let s2 = Sample {
            labels: LabelSetId::from_offset(2),
            stacktrace: StackTraceId::from_offset(2),
        };
        let s3 = Sample {
            labels: LabelSetId::from_offset(3),
            stacktrace: StackTraceId::from_offset(3),
        };
        let t1 = Some(Timestamp::new(1).unwrap());
        let t2 = Some(Timestamp::new(2).unwrap());

        o.add(s1, None, vec![1, 2, 3]);
        o.add(s1, None, vec![4, 5, 6]);
        o.add(s2, None, vec![7, 8, 9]);
        o.add(s3, t1, vec![10, 11, 12]);
        o.add(s2, t2, vec![13, 14, 15]);

        o.into_iter().for_each(|(k, ts, v)| {
            if k == s1 {
                // Observations without timestamp, these are aggregated together
                assert_eq!(v, vec![5, 7, 9]);
            } else if k == s2 {
                // Same stack with and without timestamp
                if ts.is_some() {
                    assert_eq!(v, vec![13, 14, 15]);
                    assert_eq!(ts, t2);
                } else {
                    assert_eq!(v, vec![7, 8, 9]);
                    assert!(ts.is_none());
                }
            } else if k == s3 {
                // Observation with timestamp
                assert_eq!(v, vec![10, 11, 12]);
                assert_eq!(ts, t1);
            } else {
                panic!("Unexpected key");
            }
        });
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_different_key_no_ts() {
        // These are only for test purposes. The only thing that matters is that
        // they differ
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };
        let s2 = Sample {
            labels: LabelSetId::from_offset(2),
            stacktrace: StackTraceId::from_offset(2),
        };

        let mut o = Observations::default();
        o.add(s1, None, vec![1, 2, 3]);
        // This should panic
        o.add(s2, None, vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key_no_ts() {
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };

        let mut o = Observations::default();
        o.add(s1, None, vec![1, 2, 3]);
        // This should panic
        o.add(s1, None, vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_different_key_ts() {
        // These are only for test purposes. The only thing that matters is that
        // they differ
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };
        let s2 = Sample {
            labels: LabelSetId::from_offset(2),
            stacktrace: StackTraceId::from_offset(2),
        };

        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, Some(ts), vec![1, 2, 3]);
        // This should panic
        o.add(s2, Some(ts), vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key_ts() {
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };

        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, Some(ts), vec![1, 2, 3]);
        // This should panic
        o.add(s1, Some(ts), vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_different_key_mixed() {
        // These are only for test purposes. The only thing that matters is that
        // they differ
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };
        let s2 = Sample {
            labels: LabelSetId::from_offset(2),
            stacktrace: StackTraceId::from_offset(2),
        };

        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, None, vec![1, 2, 3]);
        // This should panic
        o.add(s2, Some(ts), vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key_mixed() {
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };

        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, Some(ts), vec![1, 2, 3]);
        // This should panic
        o.add(s1, None, vec![4, 5]);
    }

    #[test]
    fn into_iter_test() {
        let mut o = Observations::default();
        // These are only for test purposes. The only thing that matters is that
        // they differ
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };
        let s2 = Sample {
            labels: LabelSetId::from_offset(2),
            stacktrace: StackTraceId::from_offset(2),
        };
        let s3 = Sample {
            labels: LabelSetId::from_offset(3),
            stacktrace: StackTraceId::from_offset(3),
        };
        let t1 = Some(Timestamp::new(1).unwrap());

        o.add(s1, None, vec![1, 2, 3]);
        o.add(s1, None, vec![4, 5, 6]);
        o.add(s2, None, vec![7, 8, 9]);
        o.add(s3, t1, vec![1, 1, 2]);

        let mut count = 0;
        o.into_iter().for_each(|(k, ts, v)| {
            count += 1;
            if k == s1 {
                assert!(ts.is_none());
                assert_eq!(v, vec![5, 7, 9]);
            } else if k == s2 {
                assert!(ts.is_none());
                assert_eq!(v, vec![7, 8, 9]);
            } else if k == s3 {
                assert_eq!(ts, t1);
                assert_eq!(v, vec![1, 1, 2]);
            } else {
                panic!("Unexpected key");
            }
        });
        // Two of the samples were aggregated, so three total samples at the end
        assert_eq!(count, 3);
    }
}
