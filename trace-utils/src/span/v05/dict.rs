// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use tinybytes::{Bytes, BytesString};

pub struct SharedDict {
    string_map: HashMap<BytesString, usize>,
    dict: Vec<BytesString>,
}

impl SharedDict {
    pub fn get_or_insert(&mut self, str: &BytesString) -> Result<u32, std::num::TryFromIntError> {
        if let Some(index) = self.string_map.get(str) {
            (*index).try_into()
        } else {
            let index = self.dict.len();
            self.dict.push(str.clone());
            self.string_map.insert(str.clone(), index);
            index.try_into()
        }
    }

    pub fn dict(&mut self) -> Vec<BytesString> {
        let dict = std::mem::take(&mut self.dict);
        self.string_map.retain(|k, _| {
            if k.as_str() == "" {
                self.dict.push(k.clone());
                true
            } else {
                false
            }
        });
        dict
    }
}

impl Default for SharedDict {
    fn default() -> Self {
        let empty_str = unsafe { BytesString::from_bytes_unchecked(Bytes::from_static(b"")) };
        Self {
            string_map: HashMap::from([(empty_str.clone(), 0)]),
            dict: vec![empty_str.clone()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_test() {
        let dict = SharedDict::default();

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

        assert_eq!(dict.string_map.len(), 1);
        assert_eq!(dict.dict.len(), 1);
        assert_eq!(dict.dict[0].as_str(), "");
    }
}
