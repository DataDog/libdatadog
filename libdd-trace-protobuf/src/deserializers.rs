// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

pub fn deserialize_null_into_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

/// Deserialize a HashMap<String, String> where null values are skipped.
pub fn deserialize_map_with_nullable_values<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<HashMap<String, Option<String>>> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(HashMap::new()),
        Some(map) => Ok(map
            .into_iter()
            .filter_map(|(k, v)| v.map(|val| (k, val)))
            .collect()),
    }
}

pub fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}

pub fn deserialize_duration<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Result<i64, D::Error> = Deserialize::deserialize(deserializer);
    match value {
        Ok(v) => {
            if v < 0 {
                return Ok(0);
            }
            Ok(v)
        }
        Err(_) => Ok(0),
    }
}
