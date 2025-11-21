// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! See the mod.rs file comment for why this module and file exists.

use super::super::Sample;
use super::timestamped_observations::TimestampedObservations;
use super::trimmed_observation::{ObservationLength, TrimmedObservation};
use crate::internal::Timestamp;
use std::collections::HashMap;
use std::io;

struct NonEmptyObservations {
    // Samples with no timestamps are aggregated in-place as each observation is added
    aggregated_data: AggregatedObservations,
    // Samples with timestamps are all separately kept (so we can know the exact values at the
    // given timestamp)
    timestamped_data: TimestampedObservations,
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
        #[allow(clippy::expect_used)]
        Self::try_new(observations_len).expect("failed to initialize observations")
    }

    pub fn try_new(observations_len: usize) -> io::Result<Self> {
        Ok(Observations {
            inner: Some(NonEmptyObservations {
                aggregated_data: AggregatedObservations::new(observations_len),
                timestamped_data: TimestampedObservations::try_new(observations_len).map_err(
                    |err| {
                        io::Error::new(
                            err.kind(),
                            format!("failed to create timestamped observations: {err}"),
                        )
                    },
                )?,
                obs_len: ObservationLength::new(observations_len),
                timestamped_samples_count: 0,
            }),
        })
    }

    pub fn add(
        &mut self,
        sample: Sample,
        timestamp: Option<Timestamp>,
        values: &[i64],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.inner.is_some(),
            "Use of add on Observations that were not initialized"
        );

        // SAFETY: we just ensured it has an item above.
        let observations = unsafe { self.inner.as_mut().unwrap_unchecked() };
        let obs_len = observations.obs_len;

        anyhow::ensure!(
            obs_len.eq(values.len()),
            "Observation length mismatch, expected {obs_len:?} values, got {} instead",
            values.len()
        );

        if let Some(ts) = timestamp {
            observations.timestamped_data.add(sample, ts, values)?;
            observations.timestamped_samples_count += 1;
        } else {
            observations.aggregated_data.add(sample, values)?;
        }

        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_none()
            || (self.aggregated_samples_count() == 0 && self.timestamped_samples_count() == 0)
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

    pub fn try_into_iter(self) -> io::Result<ObservationsIntoIter> {
        match self.inner {
            None => Ok(ObservationsIntoIter {
                it: Box::new(std::iter::empty()),
            }),
            Some(NonEmptyObservations {
                mut aggregated_data,
                timestamped_data,
                obs_len,
                ..
            }) => {
                let ts_it = timestamped_data
                    .try_into_iter()?
                    .map(|(s, t, o)| (s, Some(t), o));

                let agg_it = AggregatedObservationsIter {
                    iter: std::mem::take(&mut aggregated_data.data).into_iter(),
                    obs_len,
                };

                Ok(ObservationsIntoIter {
                    it: Box::new(ts_it.chain(agg_it)),
                })
            }
        }
    }
}

#[derive(Default)]
struct AggregatedObservations {
    obs_len: ObservationLength,
    data: HashMap<Sample, TrimmedObservation>,
}

impl AggregatedObservations {
    pub fn new(obs_len: usize) -> Self {
        AggregatedObservations {
            obs_len: ObservationLength::new(obs_len),
            data: Default::default(),
        }
    }

    fn add(&mut self, sample: Sample, values: &[i64]) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.obs_len.eq(values.len()),
            "Observation length mismatch, expected {:?} values, got {} instead",
            self.obs_len,
            values.len()
        );

        if let Some(v) = self.data.get_mut(&sample) {
            // SAFETY: This method is only way to build one of these, and we already checked the
            // length matches.
            unsafe { v.as_mut_slice(self.obs_len) }
                .iter_mut()
                .zip(values)
                .for_each(|(a, b)| *a = a.saturating_add(*b));
        } else {
            let trimmed = TrimmedObservation::new(values, self.obs_len);
            self.data.insert(sample, trimmed);
        }

        Ok(())
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[allow(dead_code)]
    fn contains_key(&self, sample: &Sample) -> bool {
        self.data.contains_key(sample)
    }

    #[allow(dead_code)]
    fn remove(&mut self, sample: &Sample) -> Option<TrimmedObservation> {
        self.data.remove(sample)
    }
}

impl Drop for AggregatedObservations {
    fn drop(&mut self) {
        let o = self.obs_len;
        self.data.drain().for_each(|(_, v)| {
            // SAFETY: The only way to build one of these is through
            // [Self::add], which already checked that the length was correct.
            unsafe { v.consume(o) };
        });
    }
}

struct AggregatedObservationsIter {
    iter: std::collections::hash_map::IntoIter<Sample, TrimmedObservation>,
    obs_len: ObservationLength,
}

impl Iterator for AggregatedObservationsIter {
    type Item = (Sample, Option<Timestamp>, Vec<i64>);

    fn next(&mut self) -> Option<Self::Item> {
        let (sample, observation) = self.iter.next()?;
        // SAFETY: The only way to build one of these is through
        // [Observations::add], which already checked that the length was correct.
        let vec = unsafe { observation.into_vec(self.obs_len) };
        Some((sample, None, vec))
    }
}

impl Drop for AggregatedObservationsIter {
    fn drop(&mut self) {
        for (_, observation) in &mut self.iter {
            // SAFETY: The only way to build one of these is through
            // [Observations::add], which already checked that the length was correct.
            unsafe { observation.consume(self.obs_len) };
        }
    }
}

pub struct ObservationsIntoIter {
    it: Box<dyn Iterator<Item = <ObservationsIntoIter as Iterator>::Item>>,
}

impl Iterator for ObservationsIntoIter {
    type Item = (Sample, Option<Timestamp>, Vec<i64>);
    fn next(&mut self) -> Option<Self::Item> {
        self.it.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::identifiable::*;
    use crate::internal::{LabelSetId, StackTraceId};
    use bolero::generator::*;
    use std::num::NonZeroI64;

    #[test]
    fn add_and_iter_test() {
        let mut o = Observations::new(3);
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

        o.add(s1, None, &[1, 2, 3]).unwrap();
        o.add(s1, None, &[4, 5, 6]).unwrap();
        o.add(s2, None, &[7, 8, 9]).unwrap();
        o.add(s3, t1, &[10, 11, 12]).unwrap();
        o.add(s2, t2, &[13, 14, 15]).unwrap();

        // 2 because they aggregate together
        assert_eq!(2, o.aggregated_samples_count());

        assert_eq!(2, o.timestamped_samples_count());

        o.try_into_iter().unwrap().for_each(|(k, ts, v)| {
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

        let mut o = Observations::new(3);
        o.add(s1, None, &[1, 2, 3]).unwrap();
        o.add(s2, None, &[4, 5]).unwrap_err();
    }

    #[test]
    fn different_lengths_panic_same_key_no_ts() {
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };

        let mut o = Observations::new(3);
        o.add(s1, None, &[1, 2, 3]).unwrap();
        o.add(s1, None, &[4, 5]).unwrap_err();
    }

    #[test]
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

        let mut o = Observations::new(3);
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, Some(ts), &[1, 2, 3]).unwrap();
        o.add(s2, Some(ts), &[4, 5]).unwrap_err();
    }

    #[test]
    fn different_lengths_panic_same_key_ts() {
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };

        let mut o = Observations::new(3);
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, Some(ts), &[1, 2, 3]).unwrap();
        o.add(s1, Some(ts), &[4, 5]).unwrap_err();
    }

    #[test]
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

        let mut o = Observations::new(3);
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, None, &[1, 2, 3]).unwrap();
        o.add(s2, Some(ts), &[4, 5]).unwrap_err();
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key_mixed() {
        let s1 = Sample {
            labels: LabelSetId::from_offset(1),
            stacktrace: StackTraceId::from_offset(1),
        };

        let mut o = Observations::new(3);
        let ts = NonZeroI64::new(1).unwrap();
        o.add(s1, Some(ts), &[1, 2, 3]).unwrap();
        // This should panic
        o.add(s1, None, &[4, 5]).unwrap();
    }

    #[test]
    fn into_iter_test() {
        let mut o = Observations::new(3);
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

        o.add(s1, None, &[1, 2, 3]).unwrap();
        o.add(s1, None, &[4, 5, 6]).unwrap();
        o.add(s2, None, &[7, 8, 9]).unwrap();
        o.add(s3, t1, &[1, 1, 2]).unwrap();

        let mut count = 0;
        o.try_into_iter().unwrap().for_each(|(k, ts, v)| {
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

    fn fuzz_inner(
        observations_len: &usize,
        ts_samples: &[(Sample, Timestamp, Vec<i64>)],
        no_ts_samples: &[(Sample, Vec<i64>)],
    ) {
        let obs_len = ObservationLength::new(*observations_len);

        let mut o = Observations::new(*observations_len);
        assert!(o.is_empty());

        let mut ts_samples_added = 0;

        for (s, ts, v) in ts_samples {
            if v.len() == *observations_len {
                o.add(*s, Some(*ts), v).unwrap();
                ts_samples_added += 1;
            } else {
                assert!(o.add(*s, Some(*ts), v).is_err());
            }
        }
        assert_eq!(o.timestamped_samples_count(), ts_samples_added);

        let mut aggregated_observations = AggregatedObservations::new(*observations_len);

        for (s, v) in no_ts_samples {
            if v.len() == *observations_len {
                o.add(*s, None, v).unwrap();
                aggregated_observations.add(*s, v).unwrap();
            } else {
                assert!(o.add(*s, None, v).is_err());
            }
        }

        assert_eq!(o.aggregated_samples_count(), aggregated_observations.len());

        let mut iter = o.try_into_iter().unwrap();
        for (expected_sample, expected_ts, expected_values) in ts_samples.iter() {
            if expected_values.len() != *observations_len {
                continue;
            }
            let (sample, ts, values) = iter.next().unwrap();
            assert_eq!(*expected_sample, sample);
            assert_eq!(*expected_ts, ts.unwrap());
            assert_eq!(*expected_values, values);
        }

        for (sample, ts, values) in iter {
            assert!(ts.is_none());
            assert!(aggregated_observations.contains_key(&sample));
            let expected_values = aggregated_observations.remove(&sample).unwrap();
            unsafe {
                let b = expected_values.into_vec(obs_len);
                assert_eq!(*b, values);
            }
        }
        assert!(aggregated_observations.is_empty());
    }

    #[test]
    fn fuzz_with_same_obs_len() {
        // TODO: Figure out sane limits for these numbers. We don't simply want to go up to
        // usize::MAX as that would result in crashes with too large Vec allocations.
        let obs_len_gen = if cfg!(miri) {
            1..=16usize
        } else {
            1..=1024usize
        };
        let num_ts_samples_gen = if cfg!(miri) {
            1..=16usize
        } else {
            1..=1024usize
        };
        let num_samples_gen = if cfg!(miri) {
            1..=16usize
        } else {
            1..=1024usize
        };

        // Generates 1. length of observations, 2. number of samples with timestamps, 3. number of
        // samples without timestamps. Then, 2 and 3 are used to generate the samples vectors
        // The body of this test simply adds these samples to the Observations and then uses the
        // iterator to check that the samples are the same as added.
        bolero::check!()
            .with_generator((obs_len_gen, num_ts_samples_gen, num_samples_gen))
            .and_then(|(observations_len, num_ts_samples, num_samples)| {
                let ts_samples = Vec::<(Sample, Timestamp, Vec<i64>)>::produce()
                    .with()
                    .values((
                        Sample::produce(),
                        Timestamp::produce(),
                        Vec::<i64>::produce().with().len(observations_len),
                    ))
                    .len(num_ts_samples);

                let no_ts_samples = Vec::<(Sample, Vec<i64>)>::produce()
                    .with()
                    .values((
                        Sample::produce(),
                        Vec::<i64>::produce().with().len(observations_len),
                    ))
                    .len(num_samples);

                (observations_len, ts_samples, no_ts_samples)
            })
            .for_each(|(observations_len, ts_samples, no_ts_samples)| {
                fuzz_inner(observations_len, ts_samples, no_ts_samples);
            });
    }

    #[test]
    fn fuzz_with_random_obs_len() {
        let num_ts_samples_gen = if cfg!(miri) {
            1..=16usize
        } else {
            1..=1024usize
        };
        let num_samples_gen = if cfg!(miri) {
            1..=16usize
        } else {
            1..=1024usize
        };

        bolero::check!()
            .with_generator((num_ts_samples_gen, num_samples_gen))
            .and_then(|(num_ts_samples, num_samples)| {
                let ts_samples = Vec::<(Sample, Timestamp, Vec<i64>)>::produce()
                    .with()
                    .values((
                        Sample::produce(),
                        Timestamp::produce(),
                        Vec::<i64>::produce(),
                    ))
                    .len(num_ts_samples);

                let no_ts_samples = Vec::<(Sample, Vec<i64>)>::produce()
                    .with()
                    .values((Sample::produce(), Vec::<i64>::produce()))
                    .len(num_samples);
                (ts_samples, no_ts_samples)
            })
            .for_each(|(ts_samples, no_ts_samples)| {
                fuzz_inner(&ts_samples[0].2.len(), ts_samples, no_ts_samples);
                // Here we also call the fuzz_inner with observation_length from samples without
                // timestamps to ensure that we cover the case where no timestamped samples are
                // added.
                fuzz_inner(&no_ts_samples[0].1.len(), ts_samples, no_ts_samples);
            });
    }
}
