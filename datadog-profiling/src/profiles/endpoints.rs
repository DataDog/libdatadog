// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{StringOffset, StringTable};
use crate::profiles::ProfileError;
use std::collections::HashMap;

/// Manages endpoint mappings for profiling.
///
/// This struct stores mappings from local root span IDs to endpoint names.
/// When samples contain a "local root span id" label, the corresponding
/// endpoint name will be automatically added as a "trace endpoint" label
/// during profile serialization.
pub struct Endpoints<'a> {
    /// Maps local root span IDs to string offsets in the string table
    mappings: HashMap<u64, StringOffset>,
    /// String table for storing endpoint names
    string_table: &'a StringTable,
}

impl<'a> Endpoints<'a> {
    /// Creates a new empty Endpoints instance.
    ///
    /// # Errors
    /// Returns an error if memory allocation fails.
    pub fn try_new(string_table: &'a StringTable) -> Result<Self, ProfileError> {
        let mappings = HashMap::new();
        Ok(Self {
            mappings,
            string_table,
        })
    }

    /// Adds a mapping from a local root span ID to an endpoint name.
    ///
    /// # Arguments
    /// * `local_root_span_id` - The span ID to map
    /// * `endpoint` - The endpoint name to associate with this span ID
    ///
    /// # Errors
    /// Returns an error if memory allocation fails.
    pub fn add_endpoint(
        &mut self,
        local_root_span_id: u64,
        endpoint: &str,
    ) -> Result<(), ProfileError> {
        self.mappings.try_reserve(1).map_err(ProfileError::from)?;
        let string_offset = self.string_table.try_intern(endpoint)?;
        self.mappings.insert(local_root_span_id, string_offset);
        Ok(())
    }

    /// Gets the string offset for the endpoint name associated with a given local root span ID.
    ///
    /// # Arguments
    /// * `local_root_span_id` - The span ID to look up
    ///
    /// # Returns
    /// The string offset if found, None otherwise
    pub fn get_endpoint(&self, local_root_span_id: u64) -> Option<StringOffset> {
        self.mappings.get(&local_root_span_id).copied()
    }

    /// Gets the string table containing endpoint names.
    pub fn strings(&self) -> &StringTable {
        &self.string_table
    }
}
