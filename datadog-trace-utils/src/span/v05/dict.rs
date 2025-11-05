// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use crate::span::SpanText;

/// This struct represents the shared dictionary used for interning all the strings belonging to a
/// v05 trace chunk.
pub struct SharedDict<T> {
    /// Map strings with their index (O(1) retrieval complexity).
    string_map: HashMap<T, usize>,
    /// Since the collection needs to be ordered an additional vector to keep the insertion order.
    dict: Vec<T>,
}

impl<T: SpanText> SharedDict<T> {
    /// Gets the index of the interned string. If the string is not part of the dictionary it is
    /// added and its corresponding index returned.
    ///
    /// # Arguments:
    ///
    /// * `str`: string to look up in the dictionary.
    pub fn get_or_insert(&mut self, s: &T) -> Result<u32, std::num::TryFromIntError> {
        if let Some(index) = self.string_map.get(s.borrow()) {
            (*index).try_into()
        } else {
            let index = self.dict.len();
            self.dict.push(s.clone());
            self.string_map.insert(s.clone(), index);
            index.try_into()
        }
    }

    /// Returns the dictionary. This method consumes the structure.
    pub fn dict(mut self) -> Vec<T> {
        std::mem::take(&mut self.dict)
    }
}

impl<T: SpanText> Default for SharedDict<T> {
    fn default() -> Self {
        Self {
            string_map: HashMap::from([(T::default(), 0)]),
            dict: vec![T::default()],
        }
    }
}

#[cfg(test)]
mod tests {
    use libdd_tinybytes::{Bytes, BytesString};

    use super::*;

    #[test]
    fn default_test() {
        let dict: SharedDict<BytesString> = SharedDict::default();

        assert_eq!(dict.string_map.len(), 1);
        assert_eq!(dict.dict.len(), 1);
    }

    #[test]
    fn get_or_insert_test() {
        let mut dict = SharedDict::default();
        unsafe {
            let _ = dict.get_or_insert(&BytesString::from_bytes_unchecked(Bytes::from_static(
                b"foo",
            )));
        };
        unsafe {
            let _ = dict.get_or_insert(&BytesString::from_bytes_unchecked(Bytes::from_static(
                b"bar",
            )));
        };

        assert_eq!(dict.string_map.len(), 3);
        assert_eq!(dict.dict.len(), 3);

        let res = dict.dict();
        assert_eq!(res.len(), 3);
        assert_eq!(res[0].as_str(), "");
        assert_eq!(res[1].as_str(), "foo");
        assert_eq!(res[2].as_str(), "bar");
    }
}
