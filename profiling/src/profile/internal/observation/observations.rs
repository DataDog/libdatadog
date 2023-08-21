// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! See the mod.rs file comment for why this module and file exists.

use super::super::{Id, Sample, SampleId};
use super::trimmed_observation::{ObservationLength, TrimmedObservation};
use crate::profile::{Dedup, FxIndexSet, Timestamp};
use std::collections::HashMap;

pub struct ObservationsIter<'a> {
    aggregated_iter: std::collections::hash_map::Iter<'a, SampleId, TrimmedObservation>,
    timestamped_iter: std::slice::Iter<'a, TrimmedTimestampedObservation>,
    parent: &'a Observations,
}

impl<'a> Iterator for ObservationsIter<'a> {
    type Item = (SampleId, Option<Timestamp>, &'a [i64]);
    fn next(&mut self) -> Option<Self::Item> {
        if let Some((sample_id, trimmed_obs)) = self.aggregated_iter.next() {
            let timestamp = None;
            let obs = unsafe { trimmed_obs.as_ref(self.parent.obs_len()) };
            Some((*sample_id, timestamp, obs))
        } else if let Some((sample_id, ts, trimmed_obs)) = self.timestamped_iter.next() {
            let timestamp = Some(*ts);
            let obs = unsafe { trimmed_obs.as_ref(self.parent.obs_len()) };
            Some((*sample_id, timestamp, obs))
        } else {
            None
        }
    }
}

type TrimmedTimestampedObservation = (SampleId, Timestamp, TrimmedObservation);

#[derive(Default)]
struct Observations {
    aggregated_data: HashMap<SampleId, TrimmedObservation>,
    timestamped_data: Vec<TrimmedTimestampedObservation>,
    obs_len: Option<ObservationLength>,
}

/// Public API
impl Observations {
    pub fn add(&mut self, sample_id: SampleId, timestamp: Option<Timestamp>, values: Vec<i64>) {
        self.check_length(&values);
        let obs_len = self.obs_len();
        if let Some(ts) = timestamp {
            let trimmed = TrimmedObservation::new(values, obs_len);
            self.timestamped_data.push((sample_id, ts, trimmed));
        } else if let Some(v) = self.aggregated_data.get_mut(&sample_id) {
            unsafe { v.as_mut(obs_len) }
                .iter_mut()
                .zip(values)
                .for_each(|(a, b)| *a += b);
        } else {
            let trimmed = TrimmedObservation::new(values, obs_len);
            self.aggregated_data.insert(sample_id, trimmed);
        }
    }

    pub fn iter(&self) -> ObservationsIter {
        let aggregated_iter = self.aggregated_data.iter();
        let timestamped_iter = self.timestamped_data.iter();
        ObservationsIter {
            aggregated_iter,
            timestamped_iter,
            parent: self,
        }
    }
}

impl Observations {
    fn check_length(&mut self, values: &Vec<i64>) {
        if let Some(obs_len) = self.obs_len {
            obs_len.assert_eq(values.len());
        } else {
            self.obs_len = Some(ObservationLength::new(values.len()));
        }
    }

    fn obs_len(&self) -> ObservationLength {
        self.obs_len
            .expect("ObservationLength to be set by this point")
    }
}

impl Drop for Observations {
    fn drop(&mut self) {
        if !self.aggregated_data.is_empty() {
            let o = self.obs_len();
            self.aggregated_data.drain().for_each(|(_, v)| {
                unsafe { v.consume(o) };
            });
        }

        if !self.timestamped_data.is_empty() {
            let o = self.obs_len();
            self.timestamped_data.drain(..).for_each(|(_, _, v)| {
                unsafe { v.consume(o) };
            });
        }
    }
}

#[cfg(test)]
mod test {
    use std::num::NonZeroI64;

    use super::*;

    #[test]
    fn add_and_iter_test() {
        let mut o = Observations::default();
        let s1 = SampleId::from_offset(1);
        let s2 = SampleId::from_offset(2);
        let s3 = SampleId::from_offset(3);
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
        let mut o = Observations::default();
        o.add(SampleId::from_offset(1), None, vec![1, 2, 3]);
        // This should panic
        o.add(SampleId::from_offset(2), None, vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key_no_ts() {
        let mut o = Observations::default();
        o.add(SampleId::from_offset(1), None, vec![1, 2, 3]);
        // This should panic
        o.add(SampleId::from_offset(1), None, vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_different_key_ts() {
        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(SampleId::from_offset(1), Some(ts), vec![1, 2, 3]);
        // This should panic
        o.add(SampleId::from_offset(2), Some(ts), vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key_ts() {
        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(SampleId::from_offset(1), Some(ts), vec![1, 2, 3]);
        // This should panic
        o.add(SampleId::from_offset(1), Some(ts), vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_different_key_mixed() {
        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(SampleId::from_offset(1), None, vec![1, 2, 3]);
        // This should panic
        o.add(SampleId::from_offset(2), Some(ts), vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key_mixed() {
        let mut o = Observations::default();
        let ts = NonZeroI64::new(1).unwrap();
        o.add(SampleId::from_offset(1), Some(ts), vec![1, 2, 3]);
        // This should panic
        o.add(SampleId::from_offset(1), None, vec![4, 5]);
    }
}
