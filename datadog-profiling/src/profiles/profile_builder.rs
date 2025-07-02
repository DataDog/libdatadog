// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    collections::{string_table::StringTable, SliceSet, Store},
    profiles::{Compressor, Endpoints, LabelsSet, ProfileError, SampleManager},
};
use datadog_profiling_protobuf::{
    Function, Label, Location, Mapping, Record, StringOffset, NO_OPT_ZERO,
};

/// A builder for constructing profiles with multiple string tables.
///
/// This builder manages the deduplication of string tables and automatically
/// adjusts string offsets when writing different components to ensure they
/// reference the correct strings in the final merged string table.
#[derive(Default)]
pub struct ProfileBuilder {
    /// Track which string tables have been written and their base offsets.
    written_string_tables: Vec<(*const StringTable, StringOffset)>,
    /// The next available string offset in the merged string table.
    next_string_offset: StringOffset,
}

impl ProfileBuilder {
    /// Creates a new ProfileBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adjusts a string offset by adding the base offset only if the string is
    /// not well-known.
    fn adjust_string_offset(base_offset: StringOffset, offset: StringOffset) -> StringOffset {
        if StringTable::is_well_known(offset) {
            offset
        } else {
            base_offset + offset - (StringTable::WELL_KNOWN_COUNT as u32)
        }
    }

    /// Adds a string table to the profile if not already added. Returns the
    /// base offset where this string table starts in the merged string table.
    fn add_string_table(
        &mut self,
        strings: &StringTable,
        compressor: &mut Compressor,
    ) -> Result<StringOffset, ProfileError> {
        let table_ptr = strings as *const StringTable;

        // Linear search through the small vec.
        for (ptr, base_offset) in &self.written_string_tables {
            if *ptr == table_ptr {
                return Ok(*base_offset);
            }
        }

        // Not found, write it now.

        // Determine if this is the first string table
        let is_first_table = self.written_string_tables.is_empty();

        // For the first table, write all strings. For subsequent tables, skip well-known strings.

        let (start_index, strings_to_add) = if is_first_table {
            // First table: write all strings
            (0, strings.len())
        } else {
            // Subsequent tables must have well-known strings.
            if strings.len() < StringTable::WELL_KNOWN_COUNT {
                return Err(ProfileError::InvalidInput);
            } else {
                // Skip well-known strings, write the rest.
                (
                    StringTable::WELL_KNOWN_COUNT,
                    strings.len() - StringTable::WELL_KNOWN_COUNT,
                )
            }
        };

        // Calculate base offset for this table
        let base_offset = self.next_string_offset;

        // Check that we can update next_string_offset before doing any work
        let new_next_offset = base_offset
            .checked_add(strings_to_add)
            .ok_or(ProfileError::StorageFull)?;

        // And that we have room for 1 more table.
        self.written_string_tables.try_reserve(1)?;

        // Write the strings to the compressor
        for string in strings.iter().skip(start_index) {
            let record = Record::<_, 6, NO_OPT_ZERO>::from(string);
            compressor.encode(record)?;
        }

        self.written_string_tables.push((table_ptr, base_offset));
        self.next_string_offset = new_next_offset;

        Ok(base_offset)
    }

    /// Adds functions to the profile using the provided string table.
    pub fn add_functions(
        &mut self,
        functions: &Store<Function>,
        strings: &StringTable,
        compressor: &mut Compressor,
    ) -> Result<(), ProfileError> {
        // Add string table once and get base offset for reuse
        let base_offset = self.add_string_table(strings, compressor)?;

        // Write functions with offset adjustment
        for function in functions.iter() {
            let adjusted_function = Function {
                name: Record::from(Self::adjust_string_offset(base_offset, function.name.value)),
                system_name: Record::from(Self::adjust_string_offset(
                    base_offset,
                    function.system_name.value,
                )),
                filename: Record::from(Self::adjust_string_offset(
                    base_offset,
                    function.filename.value,
                )),
                ..*function
            };

            let record = Record::<_, 5, NO_OPT_ZERO>::from(adjusted_function);
            compressor.encode(record)?;
        }

        Ok(())
    }

    /// Adds locations to the profile.
    pub fn add_locations(
        &mut self,
        locations: &Store<Location>,
        compressor: &mut Compressor,
    ) -> Result<(), ProfileError> {
        // Write locations - no string table needed since Location only
        // contains IDs (function_id, mapping_id) which are not string offsets
        for location in locations.iter() {
            let record = Record::<_, 4, NO_OPT_ZERO>::from(*location);
            compressor.encode(record)?;
        }

        Ok(())
    }

    /// Adds mappings to the profile using the provided string table.
    pub fn add_mappings(
        &mut self,
        mappings: &Store<Mapping>,
        strings: &StringTable,
        compressor: &mut Compressor,
    ) -> Result<(), ProfileError> {
        // Add string table once and get base offset for reuse
        let base_offset = self.add_string_table(strings, compressor)?;

        // Write mappings with offset adjustment
        for mapping in mappings.iter() {
            let adjusted_mapping = Mapping {
                filename: Record::from(Self::adjust_string_offset(
                    base_offset,
                    mapping.filename.value,
                )),
                build_id: Record::from(Self::adjust_string_offset(
                    base_offset,
                    mapping.build_id.value,
                )),
                ..*mapping
            };

            let record = Record::<_, 3, NO_OPT_ZERO>::from(adjusted_mapping);
            compressor.encode(record)?;
        }

        Ok(())
    }

    fn ensure_only_str_or_num(label: Label) {
        let Label {
            key: _key,
            str,
            num,
            ..
        } = label;

        let str: i64 = str.value.into();
        let num: i64 = num.value.into();
        if str != 0 && num != 0 {
            panic!("Profile.Label invariant violated, str: {str}, num: {num}");
        }
    }

    /// Adds samples to the profile using the provided string table.
    ///
    /// Samples with "local root span id" labels will automatically get "trace endpoint"
    /// labels added based on the endpoint mappings.
    pub fn add_samples(
        &mut self,
        samples: &SampleManager,
        labels_set: &LabelsSet,
        labels_strings: &mut StringTable,
        stack_traces: &SliceSet<u64>,
        endpoints: &Endpoints,
        compressor: &mut Compressor,
    ) -> Result<(), ProfileError> {
        // Write sample types first
        for sample_type in samples.types() {
            let record = Record::<_, 1, false>::from(*sample_type);
            compressor.encode(record)?;
        }

        // Add string tables once and get base offsets for reuse
        let endpoints_base_offset = self.add_string_table(endpoints.strings(), compressor)?;
        let labels_base_offset = self.add_string_table(labels_strings, compressor)?;

        // Write samples with label offset adjustment and endpoint enrichment
        let mut temp_labels: Vec<Record<datadog_profiling_protobuf::Label, 3, NO_OPT_ZERO>> =
            Vec::new();
        for (sample, timestamp) in samples
            .timestamped_samples(labels_set, stack_traces)
            .chain(samples.aggregated_samples(labels_set, stack_traces))
        {
            // Find local root span id in the sample labels for endpoint lookup
            let mut endpoint_str_offset: Option<StringOffset> = None;
            for label in sample.labels {
                if label.value.key.value == StringTable::LOCAL_ROOT_SPAN_ID_OFFSET
                    && label.value.num.value != 0
                {
                    // Convert i64 to u64 (same as internal profile does)
                    let span_id = label.value.num.value as u64;
                    endpoint_str_offset = endpoints.get_endpoint(span_id);
                    break;
                }
            }

            // Calculate extra labels needed.
            let has_endpoint = endpoint_str_offset.is_some() as usize;
            let has_timestamp = (timestamp != 0) as usize;

            if temp_labels
                .try_reserve(sample.labels.len() + has_endpoint + has_timestamp)
                .is_err()
            {
                return Err(ProfileError::OutOfMemory);
            }

            // Add existing labels with adjusted offsets
            for label in sample.labels {
                let adjusted_label = Record::<_, 3, NO_OPT_ZERO>::from(Label {
                    key: Record::from(Self::adjust_string_offset(
                        labels_base_offset,
                        label.value.key.value,
                    )),
                    str: Record::from(Self::adjust_string_offset(
                        labels_base_offset,
                        label.value.str.value,
                    )),
                    num: label.value.num,
                });

                Self::ensure_only_str_or_num(adjusted_label.value);
                temp_labels.push(adjusted_label);
            }

            // Add endpoint label if we found one
            if let Some(endpoint_str_offset) = endpoint_str_offset {
                let label = Record::<_, 3, NO_OPT_ZERO>::from(Label {
                    key: Record::from(StringTable::TRACE_ENDPOINT_OFFSET),
                    str: Record::from(endpoints_base_offset + endpoint_str_offset),
                    ..Default::default()
                });

                Self::ensure_only_str_or_num(label.value);
                temp_labels.push(label);
            }

            // Add timestamp label for timestamped samples
            if timestamp != 0 {
                let label = Record::<_, 3, NO_OPT_ZERO>::from(Label {
                    key: Record::from(StringTable::END_TIMESTAMP_NS_OFFSET),
                    num: Record::from(timestamp),
                    ..Default::default()
                });
                Self::ensure_only_str_or_num(label.value);
                temp_labels.push(label);
            }

            // Create and encode the sample
            let adjusted_sample = datadog_profiling_protobuf::Sample {
                location_ids: sample.location_ids,
                values: sample.values,
                labels: temp_labels.as_slice(),
            };
            let record = Record::<_, 2, false>::from(adjusted_sample);
            compressor.encode(record)?;

            temp_labels.clear();
        }

        Ok(())
    }
}
