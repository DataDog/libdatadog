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
                sample: vec![],
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
                scope: None,               // It is legal to leave this unset according to the spec
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

    // Helper functions for test setup
    fn create_basic_function() -> crate::api::Function<'static> {
        crate::api::Function {
            name: "test_function",
            system_name: "test_system",
            filename: "test_file.rs",
        }
    }

    fn create_basic_mapping() -> crate::api::Mapping<'static> {
        crate::api::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "test_binary",
            build_id: "test_build_id",
        }
    }

    fn setup_profile_with_function_and_location(
        sample_types: &[crate::api::ValueType<'static>],
    ) -> (InternalProfile, crate::api::Location<'static>) {
        let mut internal_profile = InternalProfile::new(sample_types, None);
        let function = create_basic_function();
        let mapping = create_basic_mapping();
        let location = crate::api::Location {
            mapping,
            function,
            address: 0x1000,
            line: 42,
        };

        let _function_id = internal_profile.try_add_function(&function);
        let _mapping_id = internal_profile.try_add_mapping(&mapping);
        let location_id = internal_profile.try_add_location(&location).unwrap();
        let _stack_trace_id = internal_profile.try_add_stacktrace(vec![location_id]);

        (internal_profile, location)
    }

    fn create_string_label(key: &'static str, value: &'static str) -> crate::api::Label<'static> {
        crate::api::Label {
            key,
            str: value,
            num: 0,
            num_unit: "",
        }
    }

    fn create_numeric_label(
        key: &'static str,
        value: i64,
        unit: &'static str,
    ) -> crate::api::Label<'static> {
        crate::api::Label {
            key,
            str: "",
            num: value,
            num_unit: unit,
        }
    }

    // Common assertion helpers
    fn assert_duration_calculation(profiles: &[datadog_profiling_otel::Profile]) {
        for profile in profiles {
            assert!(profile.duration_nanos > 0);
        }
    }

    fn assert_profile_has_correct_sample(
        profile: &datadog_profiling_otel::Profile,
        expected_values: Vec<i64>,
        expected_stack_index: i32,
        expected_attribute_count: usize,
    ) {
        assert_eq!(profile.sample.len(), 1);
        let sample = &profile.sample[0];
        assert_eq!(sample.values, expected_values);
        assert_eq!(sample.stack_index, expected_stack_index);
        assert_eq!(sample.attribute_indices.len(), expected_attribute_count);
    }

    fn assert_sample_has_timestamp(
        sample: &datadog_profiling_otel::Sample,
        expected_timestamp: u64,
    ) {
        assert_eq!(sample.timestamps_unix_nano.len(), 1);
        assert_eq!(sample.timestamps_unix_nano[0], expected_timestamp);
    }

    fn assert_profiles_data_structure(otel_profiles_data: &datadog_profiling_otel::ProfilesData) {
        assert!(otel_profiles_data.dictionary.is_some());
        assert_eq!(otel_profiles_data.resource_profiles.len(), 1);
        assert_eq!(
            otel_profiles_data.resource_profiles[0].scope_profiles.len(),
            1
        );
    }

    #[test]
    fn test_convert_into_otel() {
        // Test empty profile
        let internal_profile = InternalProfile::new(&[], None);
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        assert_profiles_data_structure(&otel_profiles_data);
        let dictionary = otel_profiles_data.dictionary.unwrap();
        assert_eq!(dictionary.mapping_table.len(), 0);
        assert_eq!(dictionary.location_table.len(), 0);
        assert_eq!(dictionary.function_table.len(), 0);
        assert_eq!(dictionary.stack_table.len(), 0);
        assert_eq!(dictionary.string_table.len(), 4);
        assert_eq!(dictionary.string_table[0], ""); // Empty string
        assert_eq!(dictionary.string_table[1], "local root span id");
        assert_eq!(dictionary.string_table[2], "trace endpoint");
        assert_eq!(dictionary.string_table[3], "end_timestamp_ns");
        assert_eq!(dictionary.link_table.len(), 0);
        assert_eq!(dictionary.attribute_table.len(), 0);
        assert_eq!(dictionary.attribute_units.len(), 0);

        // Test with functions
        let mut internal_profile = InternalProfile::new(&[], None);
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

        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let dictionary = otel_profiles_data.dictionary.unwrap();
        assert_eq!(dictionary.function_table.len(), 2);
        assert_eq!(dictionary.string_table.len(), 10);

        // Test with labels
        let mut internal_profile = InternalProfile::new(&[], None);
        let label1 = create_string_label("thread_id", "main");
        let label2 = create_numeric_label("memory_usage", 1024, "bytes");
        let sample = crate::api::Sample {
            locations: vec![],
            values: &[42],
            labels: vec![label1, label2],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let dictionary = otel_profiles_data.dictionary.unwrap();
        assert_eq!(dictionary.attribute_table.len(), 2);
        assert_eq!(dictionary.attribute_units.len(), 1);

        // Test with sample types
        let sample_types = [
            crate::api::ValueType::new("cpu", "nanoseconds"),
            crate::api::ValueType::new("allocations", "count"),
        ];
        let internal_profile = InternalProfile::new(&sample_types, None);
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let scope_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0];
        assert_eq!(scope_profile.profiles.len(), 2);
        assert_duration_calculation(&scope_profile.profiles);
    }

    #[test]
    fn test_sample_conversion() {
        let sample_types = [
            crate::api::ValueType::new("cpu", "nanoseconds"),
            crate::api::ValueType::new("memory", "bytes"),
        ];
        let (mut internal_profile, location) =
            setup_profile_with_function_and_location(&sample_types);

        // Test basic sample conversion
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[100, 2048],
            labels: vec![],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let scope_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0];
        assert_eq!(scope_profile.profiles.len(), 2);
        assert_profile_has_correct_sample(&scope_profile.profiles[0], vec![100], 0, 0);
        assert_profile_has_correct_sample(&scope_profile.profiles[1], vec![2048], 0, 0);

        // Test with labels
        let (mut internal_profile, location) =
            setup_profile_with_function_and_location(&[sample_types[0]]);
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[150],
            labels: vec![
                create_string_label("thread_id", "main"),
                create_numeric_label("cpu_usage", 75, "percent"),
            ],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        assert_profile_has_correct_sample(profile, vec![150], 0, 2);

        // Verify the sample's attribute indices point to correct attributes
        let sample = &profile.sample[0];
        let dictionary = &otel_profiles_data.dictionary.as_ref().unwrap();

        // Check that attribute indices are valid
        for &attr_idx in &sample.attribute_indices {
            assert!(attr_idx >= 0);
            assert!(attr_idx < dictionary.attribute_table.len() as i32);
        }

        // Verify the actual attribute content
        let attr1 = &dictionary.attribute_table[sample.attribute_indices[0] as usize];
        let attr2 = &dictionary.attribute_table[sample.attribute_indices[1] as usize];

        // One should be the string label, one should be the numeric label
        let (string_attr, numeric_attr) = if attr1.key == "thread_id" {
            (attr1, attr2)
        } else {
            (attr2, attr1)
        };

        // Verify string attribute
        assert_eq!(string_attr.key, "thread_id");
        let s = match string_attr.value.as_ref().expect("Expected Some value") {
            datadog_profiling_otel::key_value::Value::StringValue(s) => s,
            _ => panic!("Expected StringValue"),
        };
        assert_eq!(s, "main");

        // Verify numeric attribute
        assert_eq!(numeric_attr.key, "cpu_usage");
        let n = match numeric_attr.value.as_ref().expect("Expected Some value") {
            datadog_profiling_otel::key_value::Value::IntValue(n) => n,
            _ => panic!("Expected IntValue"),
        };
        assert_eq!(*n, 75);

        // Verify attribute unit mapping
        assert_eq!(dictionary.attribute_units.len(), 1);
        let unit = &dictionary.attribute_units[0];
        assert!(unit.attribute_key_strindex > 0);
        assert!(unit.unit_strindex > 0);

        // Test with timestamps
        let (mut internal_profile, location) =
            setup_profile_with_function_and_location(&[sample_types[0]]);
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[200],
            labels: vec![],
        };
        let timestamp = crate::internal::Timestamp::new(1234567890).unwrap();
        let _ = internal_profile.try_add_sample(sample, Some(timestamp));

        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        assert_eq!(profile.sample.len(), 1);
        assert_sample_has_timestamp(&profile.sample[0], 1234567890);

        // Test zero value filtering
        let (mut internal_profile, location) =
            setup_profile_with_function_and_location(&sample_types);
        let sample = crate::api::Sample {
            locations: vec![location],
            values: &[0, 1024], // 0 nanoseconds, 1024 bytes
            labels: vec![],
        };
        let _ = internal_profile.try_add_sample(sample, None);

        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let scope_profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0];
        assert_eq!(scope_profile.profiles[0].sample.len(), 0); // Zero value filtered
        assert_profile_has_correct_sample(&scope_profile.profiles[1], vec![1024], 0, 0);

        // Test multiple samples aggregation
        let (mut internal_profile, location) =
            setup_profile_with_function_and_location(&[sample_types[0]]);
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

        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        assert_eq!(profile.sample.len(), 1);
        assert_eq!(profile.sample[0].values, vec![600]); // 100 + 200 + 300

        assert_duration_calculation(&scope_profile.profiles);
    }

    #[test]
    fn test_duration_and_period() {
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];

        // Test duration calculation
        let internal_profile = InternalProfile::new(&sample_types, None);
        let explicit_duration = std::time::Duration::from_secs(5);
        let otel_profiles_data = internal_profile
            .convert_into_otel(None, Some(explicit_duration))
            .unwrap();
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        assert_eq!(profile.duration_nanos, 5_000_000_000);

        // Test with explicit end_time
        let internal_profile2 = InternalProfile::new(&sample_types, None);
        let start_time = internal_profile2.start_time;
        let end_time = start_time + std::time::Duration::from_secs(3);
        let otel_profiles_data2 = internal_profile2
            .convert_into_otel(Some(end_time), None)
            .unwrap();
        let profile2 = &otel_profiles_data2.resource_profiles[0].scope_profiles[0].profiles[0];
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
        assert_eq!(profile3.duration_nanos, 7_000_000_000);

        // Test period conversion
        let period = crate::api::Period {
            r#type: crate::api::ValueType::new("cpu", "cycles"),
            value: 1000,
        };
        let internal_profile = InternalProfile::new(&sample_types, Some(period));
        let otel_profiles_data = internal_profile.convert_into_otel(None, None).unwrap();
        let profile = &otel_profiles_data.resource_profiles[0].scope_profiles[0].profiles[0];
        assert!(profile.period_type.is_some());
        assert_eq!(profile.period, 1000);

        // Test without period
        let internal_profile_no_period = InternalProfile::new(&sample_types, None);
        let otel_profiles_data_no_period = internal_profile_no_period
            .convert_into_otel(None, None)
            .unwrap();
        let profile_no_period =
            &otel_profiles_data_no_period.resource_profiles[0].scope_profiles[0].profiles[0];
        assert!(profile_no_period.period_type.is_none());
        assert_eq!(profile_no_period.period, 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // Skip this test when running under Miri
    fn test_serialize_into_compressed_otel() {
        // Create an internal profile with sample types
        let sample_types = [crate::api::ValueType::new("cpu", "nanoseconds")];
        let (mut internal_profile, location) =
            setup_profile_with_function_and_location(&sample_types);

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
