// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{collections::string_table::StringTable, ProfileError};
use datadog_profiling_protobuf::StringOffset;
use std::collections::HashMap;

/// Manages endpoint mappings for profiling.
///
/// This struct stores mappings from local root span IDs to endpoint names.
/// When samples contain a "local root span id" label, the corresponding
/// endpoint name will be automatically added as a "trace endpoint" label
/// during profile serialization.
pub struct Endpoints {
    /// Maps local root span IDs to string offsets in the string table
    mappings: HashMap<u64, StringOffset>,
    /// String table for storing endpoint names
    strings: StringTable,
}

impl Endpoints {
    /// Creates a new empty Endpoints instance.
    ///
    /// # Errors
    /// Returns an error if memory allocation fails.
    pub fn try_new() -> Result<Self, ProfileError> {
        Ok(Self {
            mappings: HashMap::new(),
            strings: StringTable::try_new()?,
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
        let string_offset = self.strings.try_intern(endpoint)?;
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
        &self.strings
    }
}
