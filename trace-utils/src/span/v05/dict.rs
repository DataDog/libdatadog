// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use tinybytes::{Bytes, BytesString};

pub struct SharedDict {
    string_map: HashMap<BytesString, usize>,
    dict: Vec<BytesString>,
}

impl SharedDict {
    pub fn get_or_insert(&mut self, str: &BytesString) -> u32 {
        if let Some(index) = self.string_map.get(str) {
            (*index).try_into().unwrap()
        } else {
            let index = self.dict.len();
            self.dict.push(str.clone());
            self.string_map.insert(str.clone(), index);
            index.try_into().unwrap()
        }
    }

    pub fn dict(&mut self) -> Vec<BytesString> {
        self.string_map.clear();
        std::mem::take(&mut self.dict)
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
