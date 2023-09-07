// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! See the mod.rs file comment for why this module and file exists.

use super::super::Sample;
use super::trimmed_observation::{ObservationLength, TrimmedObservation};
use crate::profile::Timestamp;
use std::collections::HashMap;

struct NonEmptyObservations {
    aggregated_data: HashMap<Sample, TrimmedObservation>,
    timestamped_data: Vec<TrimmedTimestampedObservation>,
    obs_len: ObservationLength,
}

// Timestamp and TrimmedObservation are both 64bit values
// Using a 32 bit SampleId would still take 64 bits due to padding
// So just put the Sample in here
type TrimmedTimestampedObservation = (Sample, Timestamp, TrimmedObservation);

#[derive(Default)]
pub struct Observations {
    inner: Option<NonEmptyObservations>,
}

/// Public API
impl Observations {
    pub fn add(&mut self, sample: Sample, timestamp: Option<Timestamp>, values: Vec<i64>) {
        if let Some(inner) = &self.inner {
            inner.obs_len.assert_eq(values.len());
        } else {
            self.inner = Some(NonEmptyObservations {
                aggregated_data: Default::default(),
                timestamped_data: vec![],
                obs_len: ObservationLength::new(values.len()),
            });
        };

        // SAFETY: we just ensured it has an item above.
        let observations = unsafe { self.inner.as_mut().unwrap_unchecked() };
        let obs_len = observations.obs_len;

        if let Some(ts) = timestamp {
            let trimmed = TrimmedObservation::new(values, obs_len);
            observations.timestamped_data.push((sample, ts, trimmed));
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
        self.inner.is_none()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Sample, Option<Timestamp>, &[i64])> {
        self.inner.iter().flat_map(|observations| {
            let obs_len = observations.obs_len;
            let aggregated_data = observations
                .aggregated_data
                .iter()
                .map(move |(sample, obs)| (sample, None, obs));
            let timestamped_data = observations
                .timestamped_data
                .iter()
                .map(move |(sample, ts, obs)| (sample, Some(*ts), obs));
            aggregated_data
                .chain(timestamped_data)
                .map(move |(sample, ts, obs)| {
                    // SAFETY: The only way to build one of these is through
                    // [Self::add], which already checked that the length was correct.
                    (*sample, ts, unsafe { obs.as_slice(obs_len) })
                })
        })
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
            let td = std::mem::take(&mut observations.timestamped_data);
            let ad = std::mem::take(&mut observations.aggregated_data);
            let td_it = td.into_iter().map(|(s, t, o)| (s, Some(t), o));
            let ad_it = ad.into_iter().map(|(s, o)| (s, None, o));
            td_it
                .chain(ad_it)
                .map(move |(s, t, o)| (s, t, unsafe { o.into_vec(observations.obs_len) }))
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
        self.timestamped_data.drain(..).for_each(|(_, _, v)| {
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
    use crate::profile::{LabelSetId, StackTraceId};
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
        o.iter().for_each(|(k, ts, v)| {
            assert!(ts.is_none());
            if k == s1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if k == s2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else {
                panic!("Unexpected key");
            }
        });
        // Iter twice to make sure there are no issues doing that
        o.iter().for_each(|(k, ts, v)| {
            assert!(ts.is_none());
            if k == s1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if k == s2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else {
                panic!("Unexpected key");
            }
        });
        o.add(s3, t1, vec![10, 11, 12]);

        o.iter().for_each(|(k, ts, v)| {
            if k == s1 {
                assert_eq!(v, vec![5, 7, 9]);
                assert!(ts.is_none());
            } else if k == s2 {
                assert_eq!(v, vec![7, 8, 9]);
                assert!(ts.is_none());
            } else if k == s3 {
                assert_eq!(v, vec![10, 11, 12]);
                assert_eq!(ts, t1);
            } else {
                panic!("Unexpected key");
            }
        });

        o.add(s2, t2, vec![13, 14, 15]);
        o.iter().for_each(|(k, ts, v)| {
            if k == s1 {
                assert_eq!(v, vec![5, 7, 9]);
                assert!(ts.is_none());
            } else if k == s2 {
                if ts.is_some() {
                    assert_eq!(v, vec![13, 14, 15]);
                    assert_eq!(ts, t2);
                } else {
                    assert_eq!(v, vec![7, 8, 9]);
                    assert!(ts.is_none());
                }
            } else if k == s3 {
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
