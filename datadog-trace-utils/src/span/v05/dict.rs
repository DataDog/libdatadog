// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::SpanText;

/// This struct represents the shared dictionary used for interning all the strings belonging to a
/// v05 trace chunk.
#[derive(Debug, Clone)]
pub struct SharedDict<T> {
    /// Map strings with their index and keep insertion order(O(1) retrieval complexity).
    map: indexmap::IndexSet<T>,
}

impl<T: SpanText> serde::Serialize for SharedDict<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(self.map.iter().map(|entry| -> &str { entry.borrow() }))
    }
}

impl<T: SpanText> SharedDict<T> {
    /// Gets the index of the interned string. If the string is not part of the dictionary it is
    /// added and its corresponding index returned.
    ///
    /// # Arguments:
    ///
    /// * `str`: string to look up in the dictionary.
    pub fn get_or_insert(&mut self, s: T) -> Result<u32, std::num::TryFromIntError> {
        if let Some(index) = self.map.get_index_of(s.borrow()) {
            (index).try_into()
        } else {
            let index = self.map.len();
            self.map.insert(s);
            index.try_into()
        }
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.map.iter()
    }
}

impl<T: SpanText> Default for SharedDict<T> {
    fn default() -> Self {
        Self {
            map: indexmap::indexset! {T::default()},
        }
    }
}

#[cfg(test)]
mod tests {
    use tinybytes::{Bytes, BytesString};

    use super::*;

    #[test]
    fn default_test() {
        let dict: SharedDict<BytesString> = SharedDict::default();

        assert_eq!(dict.map.len(), 1);
    }

    #[test]
    fn get_or_insert_test() {
        let mut dict = SharedDict::default();
        unsafe {
            let _ = dict.get_or_insert(BytesString::from_bytes_unchecked(Bytes::from_static(
                b"foo",
            )));
        };
        unsafe {
            let _ = dict.get_or_insert(BytesString::from_bytes_unchecked(Bytes::from_static(
                b"bar",
            )));
        };

        assert_eq!(dict.map.len(), 3);

        assert_eq!(dict.map[0].as_str(), "");
        assert_eq!(dict.map[1].as_str(), "foo");
        assert_eq!(dict.map[2].as_str(), "bar");
    }
}
