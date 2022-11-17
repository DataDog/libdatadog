// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::collections::HashMap;

use serde::ser::SerializeMap;
use serde::Serialize;

#[derive(Default, PartialEq, Eq, Debug, Clone)]
pub struct ProfiledEndpointsStats {
    count: HashMap<String, i64>,
}

impl Serialize for ProfiledEndpointsStats {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.count.len()))?;
        for (k, v) in &self.count {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl From<HashMap<String, i64>> for ProfiledEndpointsStats {
    fn from(count: HashMap<String, i64>) -> Self {
        ProfiledEndpointsStats { count }
    }
}

impl ProfiledEndpointsStats {
    pub fn add_endpoint_count(&mut self, endpoint_name: String, value: i64) {
        *self.count.entry(endpoint_name).or_insert(0) += value;
    }

    pub fn is_empty(&self) -> bool {
        self.count.is_empty()
    }
}
