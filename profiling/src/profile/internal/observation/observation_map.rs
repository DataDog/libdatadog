// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! See the mod.rs file comment for why this module and file exists.

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

/// A `Map` like structure, specialized to storing profile observations in
/// a memory efficient way.
///
/// See the file comment on observations/mod.rs for more details.
pub struct ObservationMap<K: Hash + Eq> {
    data: HashMap<K, TrimmedObservation>,
    obs_len: Option<ObservationLength>,
}

impl<K: Hash + Eq> Default for ObservationMap<K> {
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
        self.check_length(&values);

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
    fn check_length(&mut self, values: &Vec<i64>) {
        if let Some(obs_len) = self.obs_len {
            obs_len.assert_eq(values.len());
        } else {
            self.obs_len = Some(ObservationLength::new(values.len()));
        }
    }

    fn _get(&self, key: &K) -> Option<&[i64]> {
        self.data.get(key).map(|v| v.as_ref(self.obs_len.unwrap()))
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut [i64]> {
        self.data
            .get_mut(key)
            .map(|v| v.as_mut(self.obs_len.unwrap()))
    }

    fn insert(&mut self, key: K, values: Vec<i64>) {
        self.check_length(&values);
        let values = TrimmedObservation::new(values, self.obs_len.unwrap());
        assert!(!self.data.contains_key(&key));
        self.data.insert(key, values);
    }

    fn obs_len(&self) -> ObservationLength {
        self.obs_len
            .expect("ObservationLength to be set before it's used")
    }
}

impl<K: Hash + Eq> Drop for ObservationMap<K> {
    fn drop(&mut self) {
        if !self.is_empty() {
            let o = self.obs_len();
            self.data.drain().for_each(|(_, v)| {
                v.consume(o);
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
        let mut tsm: ObservationMap<usize> = ObservationMap::default();
        tsm.insert_or_aggregate(1, vec![1, 2, 3]);
        // This should panic
        tsm.insert_or_aggregate(2, vec![4, 5]);
    }

    #[test]
    #[should_panic]
    fn different_lengths_panic_same_key() {
        let mut tsm: ObservationMap<usize> = ObservationMap::default();
        tsm.insert_or_aggregate(1, vec![1, 2, 3]);
        // This should panic
        tsm.insert_or_aggregate(1, vec![4, 5]);
    }

    #[test]
    fn explicit_new() {
        let mut tsm: ObservationMap<usize> = ObservationMap::new(3);
        assert!(tsm.is_empty());
        tsm.insert_or_aggregate(1, vec![1, 2, 3]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_aggregate(1, vec![4, 5, 6]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_aggregate(2, vec![7, 8, 9]);
        assert_eq!(tsm.len(), 2);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if *k == 2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else {
                panic!("Unexpected key");
            }
        });
        // Iter twice to make sure there are no issues doing that
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if *k == 2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else {
                panic!("Unexpected key");
            }
        });
        tsm.insert_or_aggregate(3, vec![10, 11, 12]);
        assert_eq!(tsm.len(), 3);
        // Iter twice to make sure there are no issues doing that
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if *k == 2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else if *k == 3 {
                assert_eq!(v, vec![10, 11, 12]);
            } else {
                panic!("Unexpected key");
            }
        });
    }

    #[test]
    fn empty_vec_test() {
        let mut tsm: ObservationMap<usize> = ObservationMap::default();

        assert!(tsm.is_empty());
        tsm.insert_or_aggregate(1, vec![]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_aggregate(1, vec![]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_aggregate(2, vec![]);
        assert_eq!(tsm.len(), 2);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 || *k == 2 {
                assert_eq!(v, Vec::<i64>::new());
            } else {
                panic!("Unexpected key");
            }
        });
    }

    #[test]
    #[should_panic]
    fn explicit_new_panics_wrong_length() {
        let mut tsm: ObservationMap<usize> = ObservationMap::new(3);
        // This should panic
        tsm.insert_or_aggregate(1, vec![1, 2]);
    }

    #[test]
    fn non_empty_vec_test() {
        let mut tsm: ObservationMap<usize> = ObservationMap::default();
        assert!(tsm.is_empty());
        tsm.insert_or_aggregate(1, vec![1, 2, 3]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_aggregate(1, vec![4, 5, 6]);
        assert_eq!(tsm.len(), 1);
        tsm.insert_or_aggregate(2, vec![7, 8, 9]);
        assert_eq!(tsm.len(), 2);
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if *k == 2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else {
                panic!("Unexpected key");
            }
        });
        // Iter twice to make sure there are no issues doing that
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if *k == 2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else {
                panic!("Unexpected key");
            }
        });
        tsm.insert_or_aggregate(3, vec![10, 11, 12]);
        assert_eq!(tsm.len(), 3);
        // Iter twice to make sure there are no issues doing that
        tsm.iter().for_each(|(k, v)| {
            if *k == 1 {
                assert_eq!(v, vec![5, 7, 9]);
            } else if *k == 2 {
                assert_eq!(v, vec![7, 8, 9]);
            } else if *k == 3 {
                assert_eq!(v, vec![10, 11, 12]);
            } else {
                panic!("Unexpected key");
            }
        });
    }
}
