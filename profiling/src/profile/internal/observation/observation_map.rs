// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use super::trimmed_observation::*;
use std::collections::HashMap;
use std::hash::Hash;

pub struct ObservationMapIter<'a, K: 'a + Hash + Eq> {
    iter: std::collections::hash_map::Iter<'a, K, TrimmedObservation>,
    obs_len: ObservationLength,
}

impl<'a, K: 'a + Hash + Eq> Iterator for ObservationMapIter<'a, K> {
    type Item = (&'a K, &'a [i64]);
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(k, v)| (k, v.as_ref(self.obs_len)))
    }
}

pub struct ObservationMap<K> {
    data: HashMap<K, TrimmedObservation>,
    obs_len: Option<ObservationLength>,
}

impl<K> Default for ObservationMap<K> {
    fn default() -> Self {
        Self {
            data: Default::default(),
            obs_len: Default::default(),
        }
    }
}

impl<K: Hash + Eq> ObservationMap<K> {
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

    pub fn iter(&self) -> ObservationMapIter<K> {
        assert!(self.is_empty() || self.obs_len.is_some());
        let iter = self.data.iter();
        let obs_len = self.obs_len.unwrap_or(ObservationLength::new(0));
        ObservationMapIter { iter, obs_len }
    }

    pub fn insert_or_aggregate(&mut self, key: K, values: Vec<i64>) {
        match self.get_mut(&key) {
            None => {
                self.insert(key, values);
            }
            Some(v) => v.iter_mut().zip(values).for_each(|(a, b)| *a += b),
        }
    }
}

// Private functions
impl<K: Hash + Eq> ObservationMap<K> {
    fn _get(&self, key: &K) -> Option<&[i64]> {
        self.data.get(key).map(|v| v.as_ref(self.obs_len.unwrap()))
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut [i64]> {
        self.data
            .get_mut(key)
            .map(|v| v.as_mut(self.obs_len.unwrap()))
    }

    fn insert(&mut self, key: K, value: Vec<i64>) {
        // Init the length at first use
        if self.obs_len.is_none() {
            self.obs_len = Some(ObservationLength::new(value.len()));
        }
        let value = TrimmedObservation::new(value, self.obs_len.unwrap());
        assert!(!self.data.contains_key(&key));
        self.data.insert(key, value);
    }
}

impl<K> Drop for ObservationMap<K> {
    fn drop(&mut self) {
        self.data.drain().for_each(|(_, v)| {
            let _ = v.into_boxed_slice(self.obs_len.unwrap());
        });
    }
}
