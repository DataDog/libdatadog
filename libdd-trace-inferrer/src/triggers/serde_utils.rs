// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serde utility functions for trigger deserialization.

use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

/// Deserializes a map that may be null in JSON into an empty HashMap.
pub fn nullable_map<'de, D, K, V>(deserializer: D) -> Result<HashMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + std::hash::Hash + Eq,
    V: Deserialize<'de>,
{
    Ok(Option::<HashMap<K, V>>::deserialize(deserializer)?.unwrap_or_default())
}
