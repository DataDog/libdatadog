// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::ProfileError;
use serde::Serialize;
use std::collections::HashMap;

/// Tracks statistics about endpoint usage in a profile.
///
/// This struct maintains counts of how many times each endpoint is seen,
/// using String to store endpoint names.
#[derive(Default, Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct EndpointStats {
    /// Maps endpoint names to their counts. This needs to be an owned String
    /// and not a StringOffset because this is used to submit the data to the
    /// exporter, so the string table is gone when this is serialized.
    counts: HashMap<String, i64>,
}

impl EndpointStats {
    /// Creates a new empty EndpointStats instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or increments the count for an endpoint.
    ///
    /// # Arguments
    /// * `endpoint_name` - The name of the endpoint
    /// * `value` - The value to add to the count
    ///
    /// # Errors
    /// Returns an error if memory allocation fails.
    pub fn add_endpoint_count(
        &mut self,
        endpoint_name: String,
        value: i64,
    ) -> Result<(), ProfileError> {
        self.counts.try_reserve(1).map_err(ProfileError::from)?;
        let entry = self.counts.entry(endpoint_name).or_insert(0);
        *entry = entry.saturating_add(value);
        Ok(())
    }

    /// Returns true if no endpoint statistics have been recorded.
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// Gets the count for a specific endpoint.
    ///
    /// # Arguments
    /// * `endpoint_name` - The name of the endpoint
    ///
    /// # Returns
    /// The count for the endpoint, or 0 if not found
    pub fn get_count(&self, endpoint_name: &str) -> i64 {
        self.counts.get(endpoint_name).copied().unwrap_or(0)
    }

    /// Gets the endpoint counts as a serializable HashMap.
    ///
    /// # Returns
    /// A reference to the HashMap mapping endpoint names to their counts
    pub fn counts(&self) -> &HashMap<String, i64> {
        &self.counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_endpoint_stats() -> Result<(), ProfileError> {
        let mut stats = EndpointStats::new();

        // Add some endpoints
        stats.add_endpoint_count("api/users".to_string(), 1)?;
        stats.add_endpoint_count("api/users".to_string(), 2)?;
        stats.add_endpoint_count("api/posts".to_string(), 3)?;

        // Check counts
        assert_eq!(stats.get_count("api/users"), 3);
        assert_eq!(stats.get_count("api/posts"), 3);
        assert_eq!(stats.get_count("non-existent"), 0); // Non-existent endpoint

        // Check raw counts
        let counts = stats.counts();
        assert_eq!(counts.get("api/users"), Some(&3));
        assert_eq!(counts.get("api/posts"), Some(&3));
        assert_eq!(counts.get("non-existent"), None);

        Ok(())
    }
}
