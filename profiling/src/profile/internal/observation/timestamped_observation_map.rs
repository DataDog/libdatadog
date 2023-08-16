// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

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
        .map(|(ts, obs)| (*ts, obs.as_ref(obs_len)))
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

pub struct TimestampedObservationMap<K> {
    data: HashMap<K, TrimmedTimestampedObservationVector>,
    obs_len: Option<ObservationLength>,
}

impl<K> Default for TimestampedObservationMap<K> {
    fn default() -> Self {
        Self {
            data: Default::default(),
            obs_len: Default::default(),
        }
    }
}

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
    /// We choose to record each as separate observations, and allow the backend to decide what to do.
    pub fn insert_or_append(&mut self, key: K, ts: Timestamp, values: Vec<i64>) {
        if self.obs_len.is_none() {
            self.obs_len = Some(ObservationLength::new(values.len()));
        }

        let timestamped = (ts, TrimmedObservation::new(values, self.obs_len.unwrap()));
        match self.data.get_mut(&key) {
            None => {
                self.data.insert(key, vec![timestamped]);
            }
            Some(v) => v.push(timestamped),
        }
    }
}

impl<K> Drop for TimestampedObservationMap<K> {
    fn drop(&mut self) {
        self.data.drain().for_each(|(_, v)| {
            v.into_iter().for_each(|(_, obs)| {
                let _ = obs.into_boxed_slice(self.obs_len.unwrap());
            })
        });
    }
}
