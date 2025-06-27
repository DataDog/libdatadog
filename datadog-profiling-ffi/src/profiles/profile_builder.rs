// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::endpoints::Endpoints;
use crate::profiles::{
    Compressor, LabelsSet, ProfileError, SampleManager, SliceSet,
};
use datadog_alloc::Box;
use datadog_profiling::{
    collections::string_table::StringTable, ProfileVoidResult,
};
use datadog_profiling_protobuf::{
    Function, Location, Mapping, Record, StringOffset, NO_OPT_ZERO,
};
use std::ptr;

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
        let new_next_offset = self
            .next_string_offset
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
        functions: &crate::profiles::Store<Function>,
        strings: &StringTable,
        compressor: &mut Compressor,
    ) -> Result<(), ProfileError> {
        // Add string table once and get base offset for reuse
        let base_offset = self.add_string_table(strings, compressor)?;

        // Write functions with offset adjustment
        for function in functions.iter() {
            let adjusted_function = Function {
                name: Record::from(base_offset + function.name.value),
                system_name: Record::from(
                    base_offset + function.system_name.value,
                ),
                filename: Record::from(base_offset + function.filename.value),
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
        locations: &crate::profiles::Store<Location>,
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
        mappings: &crate::profiles::Store<Mapping>,
        strings: &StringTable,
        compressor: &mut Compressor,
    ) -> Result<(), ProfileError> {
        // Add string table once and get base offset for reuse
        let base_offset = self.add_string_table(strings, compressor)?;

        // Write mappings with offset adjustment
        for mapping in mappings.iter() {
            let adjusted_mapping = Mapping {
                filename: Record::from(base_offset + mapping.filename.value),
                build_id: Record::from(base_offset + mapping.build_id.value),
                ..*mapping
            };

            let record = Record::<_, 3, NO_OPT_ZERO>::from(adjusted_mapping);
            compressor.encode(record)?;
        }

        Ok(())
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
        let endpoints_base_offset =
            self.add_string_table(endpoints.strings(), compressor)?;
        let labels_base_offset =
            self.add_string_table(labels_strings, compressor)?;

        // Write samples with label offset adjustment and endpoint enrichment
        let mut temp_labels = Vec::new();
        for (sample, timestamp) in samples
            .timestamped_samples(labels_set, stack_traces)
            .chain(samples.aggregated_samples(labels_set, stack_traces))
        {
            // Find local root span id in the sample labels for endpoint lookup
            let mut endpoint_str_offset: Option<StringOffset> = None;
            for label in sample.labels {
                if label.value.key.value
                    == StringTable::LOCAL_ROOT_SPAN_ID_OFFSET
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
                let adjusted_label = Record::<_, 3, NO_OPT_ZERO>::from(
                    datadog_profiling_protobuf::Label {
                        key: Record::from(
                            labels_base_offset + label.value.key.value,
                        ),
                        str: Record::from(
                            labels_base_offset
                                * !StringTable::is_well_known(
                                    label.value.str.value,
                                )
                                + label.value.str.value,
                        ),
                        num: label.value.num,
                    },
                );

                temp_labels.push(adjusted_label);
            }

            // Add endpoint label if we found one
            if let Some(endpoint_str_offset) = endpoint_str_offset {
                temp_labels.push(Record::<_, 3, NO_OPT_ZERO>::from(
                    datadog_profiling_protobuf::Label {
                        key: Record::from(StringTable::TRACE_ENDPOINT_OFFSET),
                        str: Record::from(
                            endpoints_base_offset + endpoint_str_offset,
                        ),
                        ..Default::default()
                    },
                ));
            }

            // Add timestamp label for timestamped samples
            if timestamp != 0 {
                temp_labels.push(Record::<_, 3, NO_OPT_ZERO>::from(
                    datadog_profiling_protobuf::Label {
                        key: Record::from(StringTable::END_TIMESTAMP_NS_OFFSET),
                        num: Record::from(timestamp),
                        ..Default::default()
                    },
                ));
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

// FFI interface

#[repr(C)]
pub enum ProfileBuilderNewResult {
    Ok(*mut ProfileBuilder),
    Err(ProfileError),
}

/// Creates a new ProfileBuilder.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_ProfileBuilder_new() -> ProfileBuilderNewResult {
    match Box::try_new(ProfileBuilder::default()) {
        Ok(boxed) => ProfileBuilderNewResult::Ok(Box::into_raw(boxed)),
        Err(_) => ProfileBuilderNewResult::Err(ProfileError::OutOfMemory),
    }
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_functions(
    builder: *mut ProfileBuilder,
    functions: *mut crate::profiles::Store<Function>,
    strings: *mut StringTable,
    compressor: *mut Compressor,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(functions) = functions.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(strings) = strings.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(
        builder.add_functions(functions, strings, compressor),
    )
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_locations(
    builder: *mut ProfileBuilder,
    locations: *mut crate::profiles::Store<Location>,
    compressor: *mut Compressor,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(locations) = locations.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_locations(locations, compressor))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_mappings(
    builder: *mut ProfileBuilder,
    mappings: *mut crate::profiles::Store<Mapping>,
    strings: *mut StringTable,
    compressor: *mut Compressor,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(mappings) = mappings.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(strings) = strings.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_mappings(mappings, strings, compressor))
}

/// # Safety
///
/// All pointer parameters must be valid pointers to their respective types.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_add_samples(
    builder: *mut ProfileBuilder,
    samples: *mut SampleManager,
    labels_set: *mut LabelsSet,
    labels_strings: *mut StringTable,
    stack_traces: *mut SliceSet<u64>,
    endpoints: *mut Endpoints,
    compressor: *mut Compressor,
) -> ProfileVoidResult {
    let Some(builder) = builder.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(samples) = samples.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(labels_set) = labels_set.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(labels_strings) = labels_strings.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(stack_traces) = stack_traces.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(endpoints) = endpoints.as_ref() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };
    let Some(compressor) = compressor.as_mut() else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(builder.add_samples(
        samples,
        labels_set,
        labels_strings,
        stack_traces,
        endpoints,
        compressor,
    ))
}

/// # Safety
///
/// The `builder` must be a valid pointer to a pointer to a `ProfileBuilder`.
/// `*builder` may be null (this function handles null gracefully).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfileBuilder_drop(
    builder: *mut *mut ProfileBuilder,
) {
    if let Some(ptr) = builder.as_mut() {
        let inner_ptr = *ptr;
        if !inner_ptr.is_null() {
            drop(Box::from_raw(inner_ptr));
            *ptr = ptr::null_mut();
        }
    }
}
