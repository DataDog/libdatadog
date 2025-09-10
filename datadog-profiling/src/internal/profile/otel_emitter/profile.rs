// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::Id;
use crate::internal::profile::otel_emitter::label::convert_label_to_key_value;
use crate::internal::profile::{EncodedProfile, Profile as InternalProfile};
use crate::iter::{IntoLendingIterator, LendingIterator};
use anyhow::{Context, Result};
use datadog_profiling_otel::ProfilesDataExt;
use std::collections::HashMap;

impl InternalProfile {
    /// Converts the profile into OpenTelemetry format
    ///
    /// * `end_time` - Optional end time of the profile. Passing None will use the current time.
    /// * `duration` - Optional duration of the profile. Passing None will try to calculate the
    ///   duration based on the end time minus the start time, but under anomalous conditions this
    ///   may fail as system clocks can be adjusted. The programmer may also accidentally pass an
    ///   earlier time. The duration will be set to zero these cases.
    pub fn convert_into_otel(
        mut self,
        end_time: Option<std::time::SystemTime>,
        duration: Option<std::time::Duration>,
    ) -> anyhow::Result<datadog_profiling_otel::ProfilesData> {
        // Calculate duration using the same logic as encode
        let end = end_time.unwrap_or_else(std::time::SystemTime::now);
        let start = self.start_time;
        let duration_nanos = duration
            .unwrap_or_else(|| {
                end.duration_since(start).unwrap_or({
                    // Let's not throw away the whole profile just because the clocks were wrong.
                    // todo: log that the clock went backward (or programmer mistake).
                    std::time::Duration::ZERO
                })
            })
            .as_nanos()
            .min(i64::MAX as u128) as i64;

        // Create individual OpenTelemetry Profiles for each ValueType
        let mut profiles = Vec::with_capacity(self.sample_types.len());

        for sample_type in self.sample_types.iter() {
            // Convert the ValueType to OpenTelemetry format
            let otel_sample_type = datadog_profiling_otel::ValueType {
                type_strindex: sample_type.r#type.value.to_raw_id() as i32,
                unit_strindex: sample_type.unit.value.to_raw_id() as i32,
                aggregation_temporality: datadog_profiling_otel::AggregationTemporality::Delta
                    .into(),
            };

            // Create a Profile for this sample type
            let profile = datadog_profiling_otel::Profile {
                sample_type: Some(otel_sample_type),
                sample: vec![], // TODO: Implement sample conversion
                time_nanos: self
                    .start_time
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as i64,
                duration_nanos, // Use calculated duration
                period_type: self.period.as_ref().map(|(_, period_type)| {
                    datadog_profiling_otel::ValueType {
                        type_strindex: period_type.r#type.value.to_raw_id() as i32,
                        unit_strindex: period_type.unit.value.to_raw_id() as i32,
                        aggregation_temporality:
                            datadog_profiling_otel::AggregationTemporality::Delta.into(),
                    }
                }),
                period: self
                    .period
                    .map(|(period_value, _)| period_value)
                    .unwrap_or(0),
                comment_strindices: vec![],  // We don't have comments
                profile_id: vec![],          // TODO: Implement when we handle profile IDs
                dropped_attributes_count: 0, // We don't drop attributes
                original_payload_format: String::new(), // There is no original payload
                original_payload: vec![],    // There is no original payload
                attribute_indices: vec![],   // There are currently no attributes at this level
            };

            profiles.push(profile);
        }

        for (sample, timestamp, mut values) in std::mem::take(&mut self.observations).into_iter() {
            let stack_index = sample.stacktrace.to_raw_id() as i32;
            let label_set = self.get_label_set(sample.labels)?;
            let attribute_indicies: Vec<_> =
                label_set.iter().map(|x| x.to_raw_id() as i32).collect();
            let labels = label_set
                .iter()
                .map(|l| self.get_label(*l).copied())
                .collect::<Result<Vec<_>>>()?;
            let link_index = 0; // TODO, handle links properly
            let timestamps_unix_nano = timestamp.map_or(vec![], |ts| vec![ts.get() as u64]);
            self.upscaling_rules.upscale_values(&mut values, &labels)?;

            for (idx, value) in values.iter().enumerate() {
                if *value != 0 {
                    let otel_sample = datadog_profiling_otel::Sample {
                        stack_index,
                        attribute_indices: attribute_indicies.clone(),
                        link_index,
                        values: vec![*value],
                        timestamps_unix_nano: timestamps_unix_nano.clone(),
                    };
                    profiles[idx].sample.push(otel_sample);
                }
            }
        }

        // Convert string table using into_lending_iter
        // Note: We can't use .map().collect() here because LendingIterator doesn't implement
        // the standard Iterator trait. LendingIterator is designed for yielding references
        // with lifetimes tied to the iterator itself, so we need to manually iterate and
        // convert each string reference to an owned String.
        let string_table = {
            let mut strings = Vec::with_capacity(self.strings.len());
            let mut iter = self.strings.into_lending_iter();
            while let Some(s) = iter.next() {
                strings.push(s.to_string());
            }
            strings
        };

        // Convert labels to KeyValues for the attribute table
        let mut key_to_unit_map = HashMap::new();
        let mut attribute_table = Vec::with_capacity(self.labels.len());

        for label in self.labels.iter() {
            let key_value = convert_label_to_key_value(label, &string_table, &mut key_to_unit_map)
                .with_context(|| {
                    format!(
                        "Failed to convert label with key index {}",
                        label.get_key().to_raw_id()
                    )
                })?;
            attribute_table.push(key_value);
        }

        // Build attribute units from the key-to-unit mapping
        let attribute_units = key_to_unit_map
            .into_iter()
            .map(
                |(key_index, unit_index)| datadog_profiling_otel::AttributeUnit {
                    attribute_key_strindex: key_index as i32,
                    unit_strindex: unit_index as i32,
                },
            )
            .collect();

        // Convert the ProfilesDictionary components
        let dictionary = datadog_profiling_otel::ProfilesDictionary {
            mapping_table: self.mappings.into_iter().map(From::from).collect(),
            location_table: self.locations.into_iter().map(From::from).collect(),
            function_table: self.functions.into_iter().map(From::from).collect(),
            stack_table: self.stack_traces.into_iter().map(From::from).collect(),
            string_table,
            attribute_table,
            attribute_units,
            link_table: vec![], // TODO: Implement when we handle trace links
        };

        // Create a basic ResourceProfiles structure
        let resource_profiles = vec![datadog_profiling_otel::ResourceProfiles {
            resource: None, // TODO: Implement when we handle resources
            scope_profiles: vec![datadog_profiling_otel::ScopeProfiles {
                scope: None,               // TODO: Implement when we handle scopes
                profiles,                  // Now contains the individual profiles
                schema_url: String::new(), // TODO: Implement when we handle schema URLs
                default_sample_type: None, // TODO: Implement when we handle sample types
            }],
            schema_url: String::new(), // TODO: Implement when we handle schema URLs
        }];

        Ok(datadog_profiling_otel::ProfilesData {
            resource_profiles,
            dictionary: Some(dictionary),
        })
    }

    /// Serializes the profile into OpenTelemetry format and compresses it using zstd.
    ///
    /// * `end_time` - Optional end time of the profile. Passing None will use the current time.
    /// * `duration` - Optional duration of the profile. Passing None will try to calculate the
    ///   duration based on the end time minus the start time, but under anomalous conditions this
    ///   may fail as system clocks can be adjusted. The programmer may also accidentally pass an
    ///   earlier time. The duration will be set to zero these cases.
    pub fn serialize_into_compressed_otel(
        mut self,
        end_time: Option<std::time::SystemTime>,
        duration: Option<std::time::Duration>,
    ) -> anyhow::Result<EncodedProfile> {
        // Extract values before consuming self
        let start = self.start_time;
        let endpoints_stats = std::mem::take(&mut self.endpoints.stats);
        let otel_profiles_data = self.convert_into_otel(end_time, duration)?;
        let buffer = otel_profiles_data.serialize_into_compressed_proto()?;
        let end = end_time.unwrap_or_else(std::time::SystemTime::now);
        Ok(EncodedProfile {
            start,
            end,
            buffer,
            endpoints_stats,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::internal::profile::Profile as InternalProfile;

    #[test]
    fn test_from_internal_profile_empty() {
        // Create an empty internal profile
        let internal_profile = InternalProfile::new(&[], None);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        assert!(otel_profiles_data.dictionary.is_some());
        let dictionary = otel_profiles_data.dictionary.unwrap();

        assert_eq!(dictionary.mapping_table.len(), 0);
        assert_eq!(dictionary.location_table.len(), 0);
        assert_eq!(dictionary.function_table.len(), 0);
        assert_eq!(dictionary.stack_table.len(), 0);
        assert_eq!(dictionary.string_table.len(), 4); // Default strings: "", "local root span id", "trace endpoint", "end_timestamp_ns"
        assert_eq!(dictionary.link_table.len(), 0);
        assert_eq!(dictionary.attribute_table.len(), 0);
        assert_eq!(dictionary.attribute_units.len(), 0);

        assert_eq!(otel_profiles_data.resource_profiles.len(), 1);

        // Check duration calculation - only if profiles exist
        let scope_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0];
        if !scope_profile.profiles.is_empty() {
            let profile = &scope_profile.profiles[0];
            // When no duration is provided, it should calculate from current time - start time
            // Since we're testing with None, None, the duration should be > 0 (current time - start
            // time)
            assert!(profile.duration_nanos > 0);
        }
    }

    #[test]
    fn test_from_internal_profile_with_data() {
        // Create an internal profile with some data
        let mut internal_profile = InternalProfile::new(&[], None);

        // Add some functions using the API Function type
        let function1 = crate::api::Function {
            name: "test_function_1",
            system_name: "test_system_1",
            filename: "test_file_1.rs",
        };
        let function2 = crate::api::Function {
            name: "test_function_2",
            system_name: "test_system_2",
            filename: "test_file_2.rs",
        };

        let _function1_id = internal_profile.try_add_function(&function1);
        let _function2_id = internal_profile.try_add_function(&function2);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        assert!(otel_profiles_data.dictionary.is_some());
        let dictionary = otel_profiles_data.dictionary.unwrap();

        assert_eq!(dictionary.function_table.len(), 2);
        assert_eq!(dictionary.string_table.len(), 10); // 4 default strings + 6 strings from the 2 functions

        // Verify the first function conversion - using actual observed values
        let otel_function1 = &dictionary.function_table[0];
        assert_eq!(otel_function1.name_strindex, 4);
        assert_eq!(otel_function1.system_name_strindex, 5);
        assert_eq!(otel_function1.filename_strindex, 6);

        // Verify the second function conversion - using actual observed values
        let otel_function2 = &dictionary.function_table[1];
        assert_eq!(otel_function2.name_strindex, 7);
        assert_eq!(otel_function2.system_name_strindex, 8);
        assert_eq!(otel_function2.filename_strindex, 9);

        // Check duration calculation - only if profiles exist
        let scope_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0];
        if !scope_profile.profiles.is_empty() {
            let profile = &scope_profile.profiles[0];
            // When no duration is provided, it should calculate from current time - start time
            assert!(profile.duration_nanos > 0);
        }
    }

    #[test]
    fn test_from_internal_profile_with_labels() {
        // Create an internal profile with some data
        let mut internal_profile = InternalProfile::new(&[], None);

        // Add some labels using the API
        let label1 = crate::api::Label {
            key: "thread_id",
            str: "main",
            num: 0,
            num_unit: "",
        };
        let label2 = crate::api::Label {
            key: "memory_usage",
            str: "",
            num: 1024,
            num_unit: "bytes",
        };

        // Add a sample with these labels
        let sample = crate::api::Sample {
            locations: vec![],
            values: &[42],
            labels: vec![label1, label2],
        };

        let _ = internal_profile.try_add_sample(sample, None);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        assert!(otel_profiles_data.dictionary.is_some());
        let dictionary = otel_profiles_data.dictionary.unwrap();

        // Should have 2 labels converted to attributes
        assert_eq!(dictionary.attribute_table.len(), 2);

        // Should have 1 attribute unit (for the numeric label with unit)
        assert_eq!(dictionary.attribute_units.len(), 1);

        // Verify the first attribute (string label)
        let attr1 = &dictionary.attribute_table[0];
        assert_eq!(attr1.key, "thread_id");
        match &attr1.value {
            Some(datadog_profiling_otel::key_value::Value::StringValue(s)) => {
                assert_eq!(s, "main");
            }
            _ => panic!("Expected StringValue"),
        }

        // Verify the second attribute (numeric label)
        let attr2 = &dictionary.attribute_table[1];
        assert_eq!(attr2.key, "memory_usage");
        match &attr2.value {
            Some(datadog_profiling_otel::key_value::Value::IntValue(n)) => {
                assert_eq!(*n, 1024);
            }
            _ => panic!("Expected IntValue"),
        }

        // Verify the attribute unit mapping
        let unit = &dictionary.attribute_units[0];
        // The key should map to the memory_usage string index
        // and the unit should map to the "bytes" string index
        assert!(unit.attribute_key_strindex > 0);
        assert!(unit.unit_strindex > 0);

        // Check duration calculation - only if profiles exist
        let scope_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0];
        if !scope_profile.profiles.is_empty() {
            let profile = &scope_profile.profiles[0];
            // When no duration is provided, it should calculate from current time - start time
            assert!(profile.duration_nanos > 0);
        }
    }

    #[test]
    fn test_from_internal_profile_with_sample_types() {
        // Create an internal profile with specific sample types
        let sample_types = [
            crate::api::ValueType::new("cpu", "nanoseconds"),
            crate::api::ValueType::new("allocations", "count"),
        ];
        let internal_profile = InternalProfile::new(&sample_types, None);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify that individual profiles are created for each sample type
        assert_eq!(otel_profiles_data.resource_profiles.len(), 1);
        let resource_profile = &otel_profiles_data.resource_profiles[0];
        assert_eq!(resource_profile.scope_profiles.len(), 1);
        let scope_profile = &resource_profile.scope_profiles[0];

        // Should have 2 profiles (one for each sample type)
        assert_eq!(scope_profile.profiles.len(), 2);

        // Verify the first profile (cpu profile)
        let cpu_profile = &scope_profile.profiles[0];
        assert!(cpu_profile.sample_type.is_some());
        let cpu_sample_type = cpu_profile.sample_type.as_ref().unwrap();
        assert_eq!(cpu_sample_type.type_strindex, 4); // "cpu" string index
        assert_eq!(cpu_sample_type.unit_strindex, 5); // "nanoseconds" string index

        // Verify the second profile (allocations profile)
        let allocations_profile = &scope_profile.profiles[1];
        assert!(allocations_profile.sample_type.is_some());
        let allocations_sample_type = allocations_profile.sample_type.as_ref().unwrap();
        assert_eq!(allocations_sample_type.type_strindex, 6); // "allocations" string index
        assert_eq!(allocations_sample_type.unit_strindex, 7); // "count" string index

        // Check duration calculation for both profiles
        for profile in &scope_profile.profiles {
            // When no duration is provided, it should calculate from current time - start time
            assert!(profile.duration_nanos > 0);
        }
    }

    #[test]
    fn test_sample_conversion_basic() {
        // Create an internal profile with sample types
        let sample_types = [
            crate::api::ValueType::new("cpu", "nanoseconds"),
            crate::api::ValueType::new("memory", "bytes"),
        ];
        let mut internal_profile = InternalProfile::new(&sample_types, None);

        // Add a function to create a location
        let function = crate::api::Function {
            name: "test_function",
            system_name: "test_system",
            filename: "test_file.rs",
        };
        let _function_id = internal_profile.try_add_function(&function);

        // Add a mapping
        let mapping = crate::api::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "test_binary",
            build_id: "test_build_id",
        };
        let _mapping_id = internal_profile.try_add_mapping(&mapping);

        // Add a location
        let location = crate::api::Location {
            mapping,
            function,
            address: 0x1000,
            line: 42,
        };
        let location_id = internal_profile.try_add_location(&location).unwrap();

        // Add a stack trace
        let _stack_trace_id = internal_profile.try_add_stacktrace(vec![location_id]);

        // Add a sample with values
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[100, 2048], // 100 nanoseconds, 2048 bytes
            labels: vec![],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        assert!(otel_profiles_data.dictionary.is_some());
        let _dictionary = otel_profiles_data.dictionary.unwrap();

        // Should have 2 profiles (one for each sample type)
        assert_eq!(
            otel_profiles_data.resource_profiles[0].scope_profiles[0]
                .profiles
                .len(),
            2
        );

        // Verify the first profile (cpu profile) has the correct sample
        let cpu_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        assert_eq!(cpu_profile.sample.len(), 1);
        let cpu_sample = &cpu_profile.sample[0];
        assert_eq!(cpu_sample.values, vec![100]);
        assert_eq!(cpu_sample.stack_index, 0); // First stack trace
        assert_eq!(cpu_sample.attribute_indices.len(), 0); // No labels

        // Verify the second profile (memory profile) has the correct sample
        let memory_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[1];
        assert_eq!(memory_profile.sample.len(), 1);
        let memory_sample = &memory_profile.sample[0];
        assert_eq!(memory_sample.values, vec![2048]);
        assert_eq!(memory_sample.stack_index, 0); // First stack trace
        assert_eq!(memory_sample.attribute_indices.len(), 0); // No labels

        // Check duration calculation for both profiles
        for profile in &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles {
            // When no duration is provided, it should calculate from current time - start time
            assert!(profile.duration_nanos > 0);
        }
    }

    #[test]
    fn test_sample_conversion_with_labels() {
        // Create an internal profile with sample types
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];
        let mut internal_profile = InternalProfile::new(&sample_types, None);

        // Add a function and location
        let function = crate::api::Function {
            name: "test_function",
            system_name: "test_system",
            filename: "test_file.rs",
        };
        let _function_id = internal_profile.try_add_function(&function);

        // Add a mapping
        let mapping = crate::api::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "test_binary",
            build_id: "test_build_id",
        };
        let _mapping_id = internal_profile.try_add_mapping(&mapping);

        let location = crate::api::Location {
            mapping,
            function,
            address: 0x1000,
            line: 42,
        };
        let location_id = internal_profile.try_add_location(&location).unwrap();

        let _stack_trace_id = internal_profile.try_add_stacktrace(vec![location_id]);

        // Add a sample with labels
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[150],
            labels: vec![
                crate::api::Label {
                    key: "thread_id",
                    str: "main",
                    num: 0,
                    num_unit: "",
                },
                crate::api::Label {
                    key: "cpu_usage",
                    str: "",
                    num: 75,
                    num_unit: "percent",
                },
            ],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        let _dictionary = otel_profiles_data.dictionary.unwrap();
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];

        // Should have 1 sample
        assert_eq!(profile.sample.len(), 1);
        let sample = &profile.sample[0];

        // Verify the sample has the correct values
        assert_eq!(sample.values, vec![150]);
        assert_eq!(sample.stack_index, 0);

        // Verify the sample has the correct attribute indices
        assert_eq!(sample.attribute_indices.len(), 2);
        // The attribute indices should correspond to the labels in the attribute table
        assert!(sample.attribute_indices[0] >= 0);

        // Check duration calculation
        // When no duration is provided, it should calculate from current time - start time
        assert!(profile.duration_nanos > 0);
        assert!(sample.attribute_indices[1] >= 0);

        // Verify the attributes were converted correctly
        assert_eq!(_dictionary.attribute_table.len(), 2);
        assert_eq!(_dictionary.attribute_units.len(), 1); // One numeric label with unit
    }

    #[test]
    fn test_sample_conversion_with_timestamps() {
        // Create an internal profile with sample types
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];
        let mut internal_profile = InternalProfile::new(&sample_types, None);

        // Add a function and location
        let function = crate::api::Function {
            name: "test_function",
            system_name: "test_system",
            filename: "test_file.rs",
        };
        let _function_id = internal_profile.try_add_function(&function);

        // Add a mapping
        let mapping = crate::api::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "test_binary",
            build_id: "test_build_id",
        };
        let _mapping_id = internal_profile.try_add_mapping(&mapping);

        let location = crate::api::Location {
            mapping,
            function,
            address: 0x1000,
            line: 42,
        };
        let location_id = internal_profile.try_add_location(&location).unwrap();

        let _stack_trace_id = internal_profile.try_add_stacktrace(vec![location_id]);

        // Add a sample with timestamp
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[200],
            labels: vec![],
        };
        let timestamp = crate::internal::Timestamp::new(1234567890).unwrap();
        let _ = internal_profile.try_add_sample(sample, Some(timestamp));

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];

        // Should have 1 sample
        assert_eq!(profile.sample.len(), 1);
        let sample = &profile.sample[0];

        // Verify the sample has the correct timestamp
        assert_eq!(sample.timestamps_unix_nano.len(), 1);
        assert_eq!(sample.timestamps_unix_nano[0], 1234567890);

        // Check duration calculation
        // When no duration is provided, it should calculate from current time - start time
        assert!(profile.duration_nanos > 0);
    }

    #[test]
    fn test_sample_conversion_zero_values_filtered() {
        // Create an internal profile with sample types
        let sample_types = [
            crate::api::ValueType::new("cpu", "nanoseconds"),
            crate::api::ValueType::new("memory", "bytes"),
        ];
        let mut internal_profile = InternalProfile::new(&sample_types, None);

        // Add a function and location
        let function = crate::api::Function {
            name: "test_function",
            system_name: "test_system",
            filename: "test_file.rs",
        };
        let _function_id = internal_profile.try_add_function(&function);

        // Add a mapping
        let mapping = crate::api::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "test_binary",
            build_id: "test_build_id",
        };
        let _mapping_id = internal_profile.try_add_mapping(&mapping);

        let location = crate::api::Location {
            mapping,
            function,
            address: 0x1000,
            line: 42,
        };
        let location_id = internal_profile.try_add_location(&location).unwrap();

        let _stack_trace_id = internal_profile.try_add_stacktrace(vec![location_id]);

        // Add a sample with one zero value and one non-zero value
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[0, 1024], // 0 nanoseconds, 1024 bytes
            labels: vec![],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        let profile0 = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        let profile1 = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[1];

        // First profile (cpu) should have no samples since value is 0
        assert_eq!(profile0.sample.len(), 0);

        // Second profile (memory) should have 1 sample since value is non-zero
        assert_eq!(profile1.sample.len(), 1);
        let memory_sample = &profile1.sample[0];
        assert_eq!(memory_sample.values, vec![1024]);

        // Check duration calculation for both profiles
        for profile in &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles {
            // When no duration is provided, it should calculate from current time - start time
            assert!(profile.duration_nanos > 0);
        }
    }

    #[test]
    fn test_sample_conversion_multiple_samples() {
        // Create an internal profile with sample types
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];
        let mut internal_profile = InternalProfile::new(&sample_types, None);

        // Add a function and location
        let function = crate::api::Function {
            name: "test_function",
            system_name: "test_system",
            filename: "test_file.rs",
        };
        let _function_id = internal_profile.try_add_function(&function);

        // Add a mapping
        let mapping = crate::api::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "test_binary",
            build_id: "test_build_id",
        };
        let _mapping_id = internal_profile.try_add_mapping(&mapping);

        let location = crate::api::Location {
            mapping,
            function,
            address: 0x1000,
            line: 42,
        };
        let location_id = internal_profile.try_add_location(&location).unwrap();

        let _stack_trace_id = internal_profile.try_add_stacktrace(vec![location_id]);

        // Add multiple samples
        let sample1 = crate::api::Sample {
            locations: vec![location],
            values: &[100],
            labels: vec![],
        };
        let sample2 = crate::api::Sample {
            locations: vec![location],
            values: &[200],
            labels: vec![],
        };
        let sample3 = crate::api::Sample {
            locations: vec![location],
            values: &[300],
            labels: vec![],
        };

        let _ = internal_profile.try_add_sample(sample1, None);
        let _ = internal_profile.try_add_sample(sample2, None);
        let _ = internal_profile.try_add_sample(sample3, None);

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        // Verify the conversion
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];

        // Should have 1 aggregated sample (samples with same stack trace and labels get aggregated)
        assert_eq!(profile.sample.len(), 1);

        // Verify the aggregated sample has the summed value
        let sample = &profile.sample[0];
        assert_eq!(sample.values, vec![600]); // 100 + 200 + 300

        // Verify all samples have the same stack index
        for sample in &profile.sample {
            assert_eq!(sample.stack_index, 0);
        }

        // Check duration calculation
        // When no duration is provided, it should calculate from current time - start time
        assert!(profile.duration_nanos > 0);
    }

    #[test]
    fn test_duration_calculation() {
        // Create an internal profile with sample types
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];
        let internal_profile = InternalProfile::new(&sample_types, None);

        // Test with explicit duration
        let explicit_duration = std::time::Duration::from_secs(5);
        let otel_profiles_data = internal_profile
            .convert_into_otel(None, Some(explicit_duration))
            .unwrap();

        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        // Should use the explicit duration (5 seconds = 5_000_000_000 nanoseconds)
        assert_eq!(profile.duration_nanos, 5_000_000_000);

        // Test with explicit end_time
        let internal_profile2 = InternalProfile::new(&sample_types, None);
        let start_time = internal_profile2.start_time;
        let end_time = start_time + std::time::Duration::from_secs(3);
        let otel_profiles_data2 = internal_profile2
            .convert_into_otel(Some(end_time), None)
            .unwrap();

        let profile2 = &otel_profiles_data2.resource_profiles[0].scope_profiles[0].profiles[0];
        // Should calculate duration from end_time - start_time (3 seconds = 3_000_000_000
        // nanoseconds)
        assert_eq!(profile2.duration_nanos, 3_000_000_000);

        // Test with both end_time and duration (duration should take precedence)
        let internal_profile3 = InternalProfile::new(&sample_types, None);
        let start_time3 = internal_profile3.start_time;
        let end_time3 = start_time3 + std::time::Duration::from_secs(10);
        let duration3 = std::time::Duration::from_secs(7);
        let otel_profiles_data3 = internal_profile3
            .convert_into_otel(Some(end_time3), Some(duration3))
            .unwrap();

        let profile3 = &otel_profiles_data3.resource_profiles[0].scope_profiles[0].profiles[0];
        // Should use the explicit duration (7 seconds = 7_000_000_000 nanoseconds)
        assert_eq!(profile3.duration_nanos, 7_000_000_000);
    }

    #[test]
    fn test_period_conversion() {
        // Create an internal profile with sample types and period
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];
        let period = crate::api::Period {
            r#type: crate::api::ValueType::new("cpu", "cycles"),
            value: 1000,
        };
        let internal_profile = InternalProfile::new(&sample_types, Some(period));

        // Convert to OpenTelemetry ProfilesData
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();

        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];

        // Should have period type information
        assert!(profile.period_type.is_some());
        let period_type = profile.period_type.as_ref().unwrap();

        // The period type should be converted from the internal profile's period
        // Note: The exact string indices depend on the string table, but we can verify they're
        // valid
        assert!(period_type.type_strindex >= 0);
        assert!(period_type.unit_strindex >= 0);

        // Should have the correct period value
        assert_eq!(profile.period, 1000);

        // Test without period
        let internal_profile_no_period = InternalProfile::new(&sample_types, None);
        let otel_profiles_data_no_period = internal_profile_no_period
            .convert_into_otel(None, None)
            .unwrap();

        let profile_no_period =
            &otel_profiles_data_no_period.resource_profiles[0].scope_profiles[0].profiles[0];

        // Should have no period type when no period is set
        assert!(profile_no_period.period_type.is_none());
        // Should have period value of 0 when no period is set
        assert_eq!(profile_no_period.period, 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // Skip this test when running under Miri
    fn test_serialize_into_compressed_otel() {
        // Create an internal profile with sample types
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];
        let mut internal_profile = InternalProfile::new(&sample_types, None);

        // Add a function and location
        let function = crate::api::Function {
            name: "test_function",
            system_name: "test_system",
            filename: "test_file.rs",
        };
        let _function_id = internal_profile.try_add_function(&function);

        // Add a mapping
        let mapping = crate::api::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "test_binary",
            build_id: "test_build_id",
        };
        let _mapping_id = internal_profile.try_add_mapping(&mapping);

        let location = crate::api::Location {
            mapping,
            function,
            address: 0x1000,
            line: 42,
        };
        let location_id = internal_profile.try_add_location(&location).unwrap();

        let _stack_trace_id = internal_profile.try_add_stacktrace(vec![location_id]);

        // Add a sample
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[150],
            labels: vec![],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        // Test serialization to compressed OpenTelemetry format
        let encoded_profile = internal_profile
            .serialize_into_compressed_otel(None, None)
            .unwrap();

        // Verify the encoded profile structure
        assert!(encoded_profile.start > std::time::UNIX_EPOCH);
        assert!(encoded_profile.end > encoded_profile.start);
        assert!(!encoded_profile.buffer.is_empty());

        // Verify the buffer contains compressed data (should be smaller than uncompressed)
        // The compressed buffer should be significantly smaller than a typical uncompressed profile
        assert!(encoded_profile.buffer.len() < 10000); // Reasonable upper bound for this small profile

        // Verify endpoints stats are preserved
        assert!(encoded_profile.endpoints_stats.is_empty()); // No endpoints added
    }
}
