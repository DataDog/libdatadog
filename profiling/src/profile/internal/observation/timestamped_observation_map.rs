// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! See the mod.rs file comment for why this module and file exists.

use super::trimmed_observation::*;
use crate::profile::Timestamp;
use std::collections::HashMap;
use std::hash::Hash;

type TrimmedTimestampedObservation = (Timestamp, TrimmedObservation);
type TrimmedTimestampedObservationVector = Vec<TrimmedTimestampedObservation>;
type TimestampedObservation<'a> = (Timestamp, &'a [i64]);
type TimestampedObservationVector<'a> = Vec<TimestampedObservation<'a>>;

fn transform_trimmed_vector(
    v: &[TrimmedTimestampedObservation],
    obs_len: ObservationLength,
) -> TimestampedObservationVector {
    v.iter()
        .map(|(ts, obs)| (*ts, unsafe { obs.as_ref(obs_len) }))
        .collect()
}

pub struct TimestampedObservationMapIter<'a, K: 'a + Hash + Eq> {
    iter: std::collections::hash_map::Iter<'a, K, TrimmedTimestampedObservationVector>,
    obs_len: ObservationLength,
}

impl<'a, K: 'a + Hash + Eq> Iterator for TimestampedObservationMapIter<'a, K> {
    type Item = (&'a K, TimestampedObservationVector<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|(k, v)| (k, transform_trimmed_vector(v, self.obs_len)))
    }
}

/// A `Map` like structure, specialized to storing timestamped profile
/// observations in a memory efficient way.
///
/// See the file comment on observations/mod.rs for more details.
pub struct TimestampedObservationMap<K: Hash + Eq> {
    data: HashMap<K, TrimmedTimestampedObservationVector>,
    obs_len: Option<ObservationLength>,
}

impl<K: Hash + Eq> Default for TimestampedObservationMap<K> {
    fn default() -> Self {
        Self {
            data: Default::default(),
            obs_len: Default::default(),
        }
    }
}

/// Private methods
impl<K: Hash + Eq> TimestampedObservationMap<K> {
    fn check_length(&mut self, values: &Vec<i64>) {
        if let Some(obs_len) = self.obs_len {
            obs_len.assert_eq(values.len());
        } else {
            self.obs_len = Some(ObservationLength::new(values.len()));
        }
    }

    fn obs_len(&self) -> ObservationLength {
        self.obs_len
            .expect("ObservationLength to be set before it's used")
    }
}

/// Public methods
impl<K: Hash + Eq> TimestampedObservationMap<K> {
    pub fn new(obs_len: usize) -> Self {
        Self {
            data: HashMap::new(),
            obs_len: Some(ObservationLength::new(obs_len)),
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> TimestampedObservationMapIter<K> {
        assert!(self.is_empty() || self.obs_len.is_some());
        let iter = self.data.iter();
        let obs_len = self.obs_len.unwrap_or(ObservationLength::new(0));
        TimestampedObservationMapIter { iter, obs_len }
    }

    /// NOTE: Repeated timestamps are unlikely but possible.
    /// We choose to record each as separate observations,
    /// and allow the backend to decide what to do.
    pub fn insert_or_append(&mut self, key: K, ts: Timestamp, values: Vec<i64>) {
        self.check_length(&values);

        let timestamped = (ts, TrimmedObservation::new(values, self.obs_len()));
        match self.data.get_mut(&key) {
            None => {
                self.data.insert(key, vec![timestamped]);
            }
            Some(v) => v.push(timestamped),
        }
    }
}

impl<K: Hash + Eq> Drop for TimestampedObservationMap<K> {
    fn drop(&mut self) {
        if !self.is_empty() {
            let o = self.obs_len();
            self.data.drain().for_each(|(_, v)| {
                v.into_iter().for_each(|(_, obs)| unsafe {
                    obs.consume(o);
                })
            });
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[should_panic]
    fn different_lengths_panic_different_key() {
        let ts1 = Timestamp::new(1).unwrap();
        let ts2 = Timestamp::new(2).unwrap();

        let mut tsm: TimestampedObservationMap<usize> = TimestampedObservationMap::default();
        tsm.insert_or_append(1, ts1, vec![1, 2, 3]);
        // This should panic
        tsm.insert_or_append(2, ts2, vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key() {
        let ts1 = Timestamp::new(1).unwrap();
        let ts2 = Timestamp::new(2).unwrap();

        let mut tsm: TimestampedObservationMap<usize> = TimestampedObservationMap::default();
        tsm.insert_or_append(1, ts1, vec![1, 2, 3]);
        // This should panic
        tsm.insert_or_append(1, ts2, vec![4, 5]);
    }

    #[test]
    fn explicit_new() {
        let ts1 = Timestamp::new(1).unwrap();
        let ts2 = Timestamp::new(2).unwrap();

        let mut tsm: TimestampedObservationMap<usize> = TimestampedObservationMap::new(3);
        assert!(tsm.is_empty());
        tsm.insert_or_append(1, ts1, vec![1, 2, 3]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_append(1, ts2, vec![4, 5, 6]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_append(2, ts2, vec![7, 8, 9]);
        assert_eq!(tsm.len(), 2);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], (ts1, vec![1, 2, 3].as_ref()));
                assert_eq!(v[1], (ts2, vec![4, 5, 6].as_ref()));
            } else if *k == 2 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![7, 8, 9].as_ref()));
            } else {
                panic!("Unexpected key");
            }
        });
        // Iter twice to make sure there are no issues doing that
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], (ts1, vec![1, 2, 3].as_ref()));
                assert_eq!(v[1], (ts2, vec![4, 5, 6].as_ref()));
            } else if *k == 2 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![7, 8, 9].as_ref()));
            } else {
                panic!("Unexpected key");
            }
        });
        tsm.insert_or_append(3, ts2, vec![10, 11, 12]);
        assert_eq!(tsm.len(), 3);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], (ts1, vec![1, 2, 3].as_ref()));
                assert_eq!(v[1], (ts2, vec![4, 5, 6].as_ref()));
            } else if *k == 2 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![7, 8, 9].as_ref()));
            } else if *k == 3 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![10, 11, 12].as_ref()));
            } else {
                panic!("Unexpected key");
            }
        });
    }

    #[test]
    fn empty_vec_tes() {
        let ts1 = Timestamp::new(1).unwrap();
        let ts2 = Timestamp::new(2).unwrap();

        let mut tsm: TimestampedObservationMap<usize> = TimestampedObservationMap::default();
        assert!(tsm.is_empty());
        tsm.insert_or_append(1, ts1, vec![]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_append(1, ts2, vec![]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_append(2, ts2, vec![]);
        assert_eq!(tsm.len(), 2);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v.len(), 2);
            } else if *k == 2 {
                assert_eq!(v.len(), 1);
            } else {
                panic!("Unexpected key");
            }
        });
    }

    #[test]
    #[should_panic]
    fn explicit_new_panics_wrong_length() {
        let ts1 = Timestamp::new(1).unwrap();

        let mut tsm: TimestampedObservationMap<usize> = TimestampedObservationMap::new(3);
        // This should panic
        tsm.insert_or_append(1, ts1, vec![1, 2]);
    }

    #[test]
    fn non_empty_vec_test() {
        let ts1 = Timestamp::new(1).unwrap();
        let ts2 = Timestamp::new(2).unwrap();

        let mut tsm: TimestampedObservationMap<usize> = TimestampedObservationMap::default();
        assert!(tsm.is_empty());
        tsm.insert_or_append(1, ts1, vec![1, 2, 3]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_append(1, ts2, vec![4, 5, 6]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_append(2, ts2, vec![7, 8, 9]);
        assert_eq!(tsm.len(), 2);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], (ts1, vec![1, 2, 3].as_ref()));
                assert_eq!(v[1], (ts2, vec![4, 5, 6].as_ref()));
            } else if *k == 2 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![7, 8, 9].as_ref()));
            } else {
                panic!("Unexpected key");
            }
        });
        // Iter twice to make sure there are no issues doing that
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], (ts1, vec![1, 2, 3].as_ref()));
                assert_eq!(v[1], (ts2, vec![4, 5, 6].as_ref()));
            } else if *k == 2 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![7, 8, 9].as_ref()));
            } else {
                panic!("Unexpected key");
            }
        });
        tsm.insert_or_append(3, ts2, vec![10, 11, 12]);
        assert_eq!(tsm.len(), 3);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], (ts1, vec![1, 2, 3].as_ref()));
                assert_eq!(v[1], (ts2, vec![4, 5, 6].as_ref()));
            } else if *k == 2 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![7, 8, 9].as_ref()));
            } else if *k == 3 {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0], (ts2, vec![10, 11, 12].as_ref()));
            } else {
                panic!("Unexpected key");
            }
        });
    }
}
