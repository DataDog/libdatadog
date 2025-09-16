// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::{Dedup, Id};
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

        // If we have span labels, figure out the corresponding endpoint labels
        // Split into two steps to avoid mutating the map we're iterating over
        let endpoint_labels: Vec<_> = self
            .labels
            .iter()
            .enumerate()
            .filter(|(_, label)| label.get_key() == self.endpoints.local_root_span_id_label)
            .filter_map(|(idx, label)| {
                self.get_endpoint_for_label(label)
                    .ok()
                    .flatten()
                    .map(|endpoint_label| (idx, endpoint_label))
            })
            .collect();

        let endpoint_labels_idx: HashMap<usize, _> = endpoint_labels
            .into_iter()
            .map(|(idx, endpoint_label)| {
                let endpoint_idx = self.labels.dedup(endpoint_label);
                (idx, endpoint_idx)
            })
            .collect();

        for (sample, timestamp, mut values) in std::mem::take(&mut self.observations).into_iter() {
            let stack_index = sample.stacktrace.to_raw_id() as i32;
            let label_set = self.get_label_set(sample.labels)?;
            let attribute_indicies: Vec<_> = label_set
                .iter()
                .map(|x| x.to_raw_id() as i32)
                .chain(
                    label_set
                        .iter()
                        .find_map(|k| endpoint_labels_idx.get(&(k.to_raw_id() as usize)))
                        .map(|label| label.to_raw_id() as i32),
                )
                .collect();
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
