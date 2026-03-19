// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry OTLP profile types for the [profiles proto v1development].
//!
//! This module re-exports the generated protobuf types from [opentelemetry-proto]
//! for the [OpenTelemetry profiles] format. The wire format is defined by the
//! [profiles.proto] specification and is distinct from the pprof format (see
//! [libdd-profiling-protobuf]); OTEL profiles use a dictionary-based layout
//! with `ProfilesData`, `ProfilesDictionary`, `ResourceProfiles`, `ScopeProfiles`,
//! and per-profile `Profile` / `Sample` types.
//!
//! [profiles proto v1development]: https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/profiles/v1development/profiles.proto
//! [opentelemetry-proto]: https://crates.io/crates/opentelemetry-proto
//! [OpenTelemetry profiles]: https://opentelemetry.io/docs/specs/otlp/
//! [profiles.proto]: https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/profiles/v1development/profiles.proto
//! [libdd-profiling-protobuf]: https://github.com/DataDog/libdatadog/tree/main/libdd-profiling-protobuf

pub use opentelemetry_proto::tonic::profiles::v1development::*;

/// Re-export of [`prost::Message`] for encoding/decoding OTEL profile types.
pub use prost::Message;

#[cfg(test)]
mod tests {
    use super::*;

    /// Example: multiple samples with different stacks (main→foo vs foo→bar), using the shared
    /// dictionary.
    #[test]
    fn roundtrip_profiles_data_with_different_stacks() {
        // string_table[0] must be "" per spec; then frame names and sample-type names.
        let string_table = vec![
            String::new(),
            "main".to_string(),
            "foo".to_string(),
            "bar".to_string(),
            "cpu".to_string(),
            "nanoseconds".to_string(),
            "allocations".to_string(),
            "count".to_string(),
        ];
        // Functions: index 0 = default; 1 = main, 2 = foo, 3 = bar (name_strindex into
        // string_table).
        let function_table = vec![
            Function::default(),
            Function {
                name_strindex: 1,
                ..Default::default()
            },
            Function {
                name_strindex: 2,
                ..Default::default()
            },
            Function {
                name_strindex: 3,
                ..Default::default()
            },
        ];
        // Locations: 0 = default; 1 = main, 2 = foo, 3 = bar (one line each, pointing at function
        // 1,2,3).
        let location_table = vec![
            Location::default(),
            Location {
                line: vec![Line {
                    function_index: 1,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Location {
                line: vec![Line {
                    function_index: 2,
                    ..Default::default()
                }],
                ..Default::default()
            },
            Location {
                line: vec![Line {
                    function_index: 3,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];
        // Stacks: 0 = default; 1 = leaf foo (loc 2) → caller main (loc 1); 2 = leaf bar (loc 3) →
        // caller foo (loc 2).
        let stack_table = vec![
            Stack::default(),
            Stack {
                location_indices: vec![2, 1],
            },
            Stack {
                location_indices: vec![3, 2],
            },
        ];

        let dictionary = ProfilesDictionary {
            mapping_table: vec![Mapping::default()],
            location_table,
            function_table,
            link_table: vec![Link::default()],
            string_table,
            attribute_table: vec![],
            stack_table,
        };

        // Two profiles with different sample types: CPU (cpu/nanoseconds) and allocations
        // (allocations/count).
        let cpu_profile = Profile {
            sample_type: Some(ValueType {
                type_strindex: 4,
                unit_strindex: 5,
                ..Default::default()
            }),
            sample: vec![
                Sample {
                    stack_index: 1,
                    values: vec![100], // 100ns on stack main→foo
                    ..Default::default()
                },
                Sample {
                    stack_index: 2,
                    values: vec![200], // 200ns on stack foo→bar
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let alloc_profile = Profile {
            sample_type: Some(ValueType {
                type_strindex: 6,
                unit_strindex: 7,
                ..Default::default()
            }),
            sample: vec![
                Sample {
                    stack_index: 1,
                    values: vec![5], // 5 allocations on stack main→foo
                    ..Default::default()
                },
                Sample {
                    stack_index: 2,
                    values: vec![12], // 12 allocations on stack foo→bar
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let scope_profiles = ScopeProfiles {
            profiles: vec![cpu_profile, alloc_profile],
            ..Default::default()
        };
        let resource_profiles = ResourceProfiles {
            scope_profiles: vec![scope_profiles],
            ..Default::default()
        };
        let data = ProfilesData {
            resource_profiles: vec![resource_profiles],
            dictionary: Some(dictionary),
        };

        let encoded = data.encode_to_vec();
        let decoded = ProfilesData::decode(encoded.as_slice()).expect("decode");

        let dict = decoded.dictionary.as_ref().expect("dictionary");
        assert_eq!(dict.string_table.len(), 8);
        assert_eq!(dict.string_table[1], "main");
        assert_eq!(dict.string_table[2], "foo");
        assert_eq!(dict.string_table[3], "bar");
        assert_eq!(dict.string_table[4], "cpu");
        assert_eq!(dict.string_table[5], "nanoseconds");
        assert_eq!(dict.string_table[6], "allocations");
        assert_eq!(dict.string_table[7], "count");

        assert_eq!(dict.stack_table.len(), 3);
        assert_eq!(dict.stack_table[1].location_indices, [2, 1]);
        assert_eq!(dict.stack_table[2].location_indices, [3, 2]);

        let scope = &decoded.resource_profiles[0].scope_profiles[0];
        assert_eq!(scope.profiles.len(), 2);

        let cpu_prof = &scope.profiles[0];
        assert_eq!(cpu_prof.sample_type.as_ref().unwrap().type_strindex, 4);
        assert_eq!(cpu_prof.sample.len(), 2);
        assert_eq!(cpu_prof.sample[0].values, [100]);
        assert_eq!(cpu_prof.sample[1].values, [200]);

        let alloc_prof = &scope.profiles[1];
        assert_eq!(alloc_prof.sample_type.as_ref().unwrap().type_strindex, 6);
        assert_eq!(alloc_prof.sample.len(), 2);
        assert_eq!(alloc_prof.sample[0].values, [5]);
        assert_eq!(alloc_prof.sample[1].values, [12]);
    }
}
