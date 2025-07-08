// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    collections::{string_table::StringTable, SliceSet, Store},
    profiles::{Compressor, EndpointStats, Endpoints, LabelsSet, ProfileError, SampleManager},
};
use datadog_profiling_protobuf::{
    Function, Label, Location, Mapping, Record, StringOffset, ValueType, NO_OPT_ZERO,
};
use std::time::SystemTime;

/// A profile that has been encoded into the pprof format.
#[derive(Debug)]
pub struct EncodedProfile {
    /// The start time of the profile
    pub start: SystemTime,
    /// The end time of the profile
    pub end: SystemTime,
    /// The compressed pprof data
    pub buffer: Vec<u8>,
    /// Statistics about endpoints seen in the profile
    pub endpoints_stats: EndpointStats,
}

/// A builder for constructing profiles with multiple string tables.
///
/// This builder manages the deduplication of string tables and automatically
/// adjusts string offsets when writing different components to ensure they
/// reference the correct strings in the final merged string table.
pub struct ProfileBuilder<'a> {
    /// Track which string tables have been written and their base offsets.
    written_string_tables: Vec<(&'a StringTable, StringOffset)>,
    /// The next available string offset in the merged string table.
    written: StringOffset,
    /// Statistics about endpoints seen during profile building
    endpoint_stats: EndpointStats,
    /// The compressor used to build the profile
    compressor: Compressor,
    /// The start time of the profile
    start_time: SystemTime,
}

impl<'a> ProfileBuilder<'a> {
    /// Creates a new ProfileBuilder.
    ///
    /// # Arguments
    /// * `start_time` - The start time for the profile.
    pub fn new(start_time: SystemTime) -> Self {
        const INITIAL_PPROF_BUFFER_SIZE: usize = 32 * 1024;
        Self {
            written_string_tables: Vec::new(),
            written: StringOffset::ZERO,
            endpoint_stats: EndpointStats::new(),
            compressor: Compressor::with_max_capacity(INITIAL_PPROF_BUFFER_SIZE),
            start_time,
        }
    }

    /// Adjusts a string offset by adding the base offset only if the string is
    /// not well-known.
    fn adjust_string_offset(base_offset: StringOffset, offset: StringOffset) -> StringOffset {
        if StringTable::is_well_known(offset) {
            offset
        } else {
            base_offset + offset
        }
    }

    /// Adds a string table to the profile if not already added. Returns the
    /// base offset where this string table starts in the merged string table.
    fn add_string_table(&mut self, strings: &'a StringTable) -> Result<StringOffset, ProfileError> {
        // Linear search through the small vec.
        for (ptr, base_offset) in &self.written_string_tables {
            if core::ptr::eq(*ptr, strings) {
                return Ok(*base_offset);
            }
        }

        // Not found, write it now.

        // For the first table, write all the well-known strings first so we
        // can handle all the tables (mostly) equally.
        let is_first = self.written_string_tables.is_empty();
        if is_first {
            for string in strings.iter().take(StringTable::WELL_KNOWN_COUNT) {
                let record = Record::<_, 6, NO_OPT_ZERO>::from(string);
                self.compressor.encode(record)?;
            }
            self.written = StringOffset::new(StringTable::WELL_KNOWN_COUNT as u32);
        }

        // This is how many non-well-known strings are being added.
        let additional = strings.len().wrapping_sub(StringTable::WELL_KNOWN_COUNT);

        // This is the amount that we add to an index of a string from this
        // table, except for well-known strings which are always fixed.
        let base_offset = if is_first {
            StringOffset::ZERO
        } else {
            self.written - (StringTable::WELL_KNOWN_COUNT as u32)
        };

        // Well-known: 5
        // Table 1: 8 strings, 3 unique + 5 well-known. Offset: 0.
        // Table 2: 6 strings, 1 unique + 5 well-known. Index 6 becomes 1 (after well known), plus 8
        // from table 1. So 9. Table 3: 6 strings, 1 unique + 5 well-known. Offset: 9-5=4.

        // Check that we can update next_string_offset before doing any work.
        let new_next_offset = self
            .written
            .checked_add(additional)
            .ok_or(ProfileError::StorageFull)?;

        // And that we have room for 1 more table.
        self.written_string_tables.try_reserve(1)?;

        // Write the strings to the compressor
        for string in strings.iter().skip(StringTable::WELL_KNOWN_COUNT) {
            let record = Record::<_, 6, NO_OPT_ZERO>::from(string);
            self.compressor.encode(record)?;
        }

        self.written_string_tables.push((strings, base_offset));
        self.written = new_next_offset;

        Ok(base_offset)
    }

    /// Adds functions to the profile using the provided string table.
    pub fn add_functions(
        &mut self,
        functions: &Store<Function>,
        strings: &'a StringTable,
    ) -> Result<(), ProfileError> {
        let base_offset = self.add_string_table(strings)?;

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
            self.compressor.encode(record)?;
        }

        Ok(())
    }

    /// Adds locations to the profile.
    pub fn add_locations(&mut self, locations: &Store<Location>) -> Result<(), ProfileError> {
        for location in locations.iter() {
            let record = Record::<_, 4, NO_OPT_ZERO>::from(*location);
            self.compressor.encode(record)?;
        }

        Ok(())
    }

    /// Adds mappings to the profile using the provided string table.
    pub fn add_mappings(
        &mut self,
        mappings: &Store<Mapping>,
        strings: &'a StringTable,
    ) -> Result<(), ProfileError> {
        let base_offset = self.add_string_table(strings)?;

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
            self.compressor.encode(record)?;
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
        labels_strings: &'a mut StringTable,
        stack_traces: &SliceSet<u64>,
        endpoints: &'a Endpoints,
    ) -> Result<(), ProfileError> {
        let labels_base_offset = self.add_string_table(labels_strings)?;
        let endpoints_base_offset = self.add_string_table(endpoints.strings())?;

        // Write sample types with adjusted string offsets
        for sample_type in samples.types() {
            let adjusted_type = ValueType {
                r#type: Record::from(Self::adjust_string_offset(
                    labels_base_offset,
                    sample_type.r#type.value,
                )),
                unit: Record::from(Self::adjust_string_offset(
                    labels_base_offset,
                    sample_type.unit.value,
                )),
            };
            let record = Record::<_, 1, false>::from(adjusted_type);
            self.compressor.encode(record)?;
        }

        // Write samples with label offset adjustment and endpoint enrichment
        let mut temp_labels: Vec<Record<Label, 3, NO_OPT_ZERO>> = Vec::new();
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

                temp_labels.push(adjusted_label);
            }

            // Add endpoint label if we found one
            if let Some(endpoint_str_offset) = endpoint_str_offset {
                self.endpoint_stats
                    .add_endpoint_count(endpoint_str_offset, 1)?;

                let label = Record::<_, 3, NO_OPT_ZERO>::from(Label {
                    key: Record::from(StringTable::TRACE_ENDPOINT_OFFSET),
                    str: Record::from(Self::adjust_string_offset(
                        endpoints_base_offset,
                        endpoint_str_offset,
                    )),
                    ..Default::default()
                });

                temp_labels.push(label);
            }

            // Add timestamp label for timestamped samples
            if timestamp != 0 {
                let label = Record::<_, 3, NO_OPT_ZERO>::from(Label {
                    key: Record::from(StringTable::END_TIMESTAMP_NS_OFFSET),
                    num: Record::from(timestamp),
                    ..Default::default()
                });
                temp_labels.push(label);
            }

            // Create and encode the sample
            let adjusted_sample = datadog_profiling_protobuf::Sample {
                location_ids: sample.location_ids,
                values: sample.values,
                labels: temp_labels.as_slice(),
            };
            let record = Record::<_, 2, false>::from(adjusted_sample);
            self.compressor.encode(record)?;

            temp_labels.clear();
        }

        Ok(())
    }

    /// Builds the profile, consuming the builder and returning an EncodedProfile.
    ///
    /// # Arguments
    /// * `end_time` - Optional end time for the profile. If None, uses the current time.
    pub fn build(mut self, end_time: Option<SystemTime>) -> Result<EncodedProfile, ProfileError> {
        Ok(EncodedProfile {
            start: self.start_time,
            end: end_time.unwrap_or_else(SystemTime::now),
            buffer: self.compressor.finish()?,
            endpoints_stats: std::mem::take(&mut self.endpoint_stats),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{samples::Sample, StackTraceSet};
    use datadog_profiling_protobuf::{prost_impls, Label, Record};
    use prost::Message;

    #[test]
    fn test_endpoint_string_offset_adjustment() {
        let mut builder = ProfileBuilder::new(SystemTime::now());

        // Create endpoints with a few endpoint names
        let mut endpoints = Endpoints::try_new().unwrap();
        endpoints.add_endpoint(10, "endpoint_10").unwrap();
        endpoints.add_endpoint(20, "endpoint_20").unwrap();

        // Create a labels string table
        let mut labels_strings = StringTable::new();

        // Create a sample with local root span ID that matches our endpoint
        let mut labels_set = LabelsSet::new();
        let value_type = ValueType {
            r#type: Record::from(labels_strings.intern("samples")),
            unit: Record::from(labels_strings.intern("count")),
        };
        let mut samples = SampleManager::new([value_type].iter().map(|x| Ok(*x))).unwrap();

        let sample_labels = vec![
            Label {
                key: Record::from(StringTable::LOCAL_ROOT_SPAN_ID_OFFSET),
                num: Record::from(10i64), // This matches our endpoint mapping
                str: Record::default(),
            },
            Label {
                key: Record::from(labels_strings.intern("thread_id")),
                str: Record::default(),
                num: Record::from(123i64),
            },
        ];

        let labels_id = labels_set.insert(&sample_labels).unwrap();
        let values = [1i64];

        // Create a stack trace set and get a valid slice ID
        let mut stack_traces = StackTraceSet::new();
        let stack_trace_range = stack_traces.insert(&[42]).unwrap();
        let stack_trace_id = stack_trace_range.into();

        let sample = Sample {
            stack_trace_id,
            values: &values,
            labels: labels_id.into(),
            timestamp: 0,
        };
        samples.add_sample(sample).unwrap();

        // Add samples to the profile
        builder
            .add_samples(
                &samples,
                &labels_set,
                &mut labels_strings,
                &stack_traces,
                &endpoints,
            )
            .unwrap();

        // Get the compressed bytes, decompress them, and decode into a Profile
        let compressed = builder.compressor.finish().unwrap();
        let mut decoder = lz4_flex::frame::FrameDecoder::new(compressed.as_slice());
        let mut decompressed = Vec::new();
        std::io::copy(&mut decoder, &mut decompressed).unwrap();
        let profile = prost_impls::Profile::decode(decompressed.as_slice()).unwrap();

        // Verify the string table layout:
        // - Well-known strings (0-4)
        // - Labels strings ("thread_id", "endpoint_10")
        assert_eq!(profile.string_table[0], "");
        assert_eq!(profile.string_table[1], "end_timestamp_ns");
        assert_eq!(profile.string_table[2], "local root span id");
        assert_eq!(profile.string_table[3], "trace endpoint");
        assert_eq!(profile.string_table[4], "span id");
        assert!(profile.string_table.contains(&"thread_id".to_string()));
        assert!(profile.string_table.contains(&"endpoint_10".to_string()));

        // Verify the sample type strings are present and correct
        assert!(profile.string_table.contains(&"samples".to_string()));
        assert!(profile.string_table.contains(&"count".to_string()));

        // Verify the sample types in the profile
        assert_eq!(profile.sample_types.len(), 1);
        let sample_type = &profile.sample_types[0];
        assert_eq!(
            profile.string_table[sample_type.r#type as usize], "samples",
            "Sample type should be 'samples'"
        );
        assert_eq!(
            profile.string_table[sample_type.unit as usize], "count",
            "Sample unit should be 'count'"
        );

        // Verify the endpoint label in the sample
        let sample = &profile.samples[0];
        let endpoint_label = sample
            .labels
            .iter()
            .find(|l| l.key == i64::from(StringTable::TRACE_ENDPOINT_OFFSET))
            .expect("Should have an endpoint label");
        assert_eq!(
            profile.string_table[endpoint_label.str as usize],
            "endpoint_10"
        );

        // Verify the thread_id label is numeric
        let thread_id_label = sample
            .labels
            .iter()
            .find(|l| {
                let key_idx = profile.string_table[l.key as usize].as_str();
                key_idx == "thread_id"
            })
            .expect("Should have a thread_id label");
        assert_eq!(thread_id_label.num, 123);
    }
}
