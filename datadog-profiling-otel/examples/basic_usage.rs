// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// This example demonstrates how to use the generated protobuf code
// from the OpenTelemetry profiles.proto file.

fn main() {
    use datadog_profiling_otel::*;

    // Create a simple profile with some basic data
    let mut profiles_dict = ProfilesDictionary::default();

    // Add some strings to the string table
    profiles_dict.string_table.push("cpu".to_string());
    profiles_dict.string_table.push("nanoseconds".to_string());
    profiles_dict.string_table.push("main".to_string());

    // Create a sample type
    let sample_type = ValueType {
        type_strindex: 0, // "cpu"
        unit_strindex: 1, // "nanoseconds"
        aggregation_temporality: AggregationTemporality::Delta.into(),
    };

    // Create a profile
    let profile = Profile {
        sample_type: Some(sample_type),
        time_nanos: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64,
        ..Default::default()
    };

    // Create profiles data
    let mut profiles_data = ProfilesData {
        dictionary: Some(profiles_dict),
        ..Default::default()
    };

    let mut scope_profiles = ScopeProfiles::default();
    scope_profiles.profiles.push(profile);

    let mut resource_profiles = ResourceProfiles::default();
    resource_profiles.scope_profiles.push(scope_profiles);

    profiles_data.resource_profiles.push(resource_profiles);

    println!("Successfully created OpenTelemetry profile data!");
    println!(
        "Profile contains {} resource profiles",
        profiles_data.resource_profiles.len()
    );
    println!(
        "Time: {}",
        profiles_data.resource_profiles[0].scope_profiles[0].profiles[0].time_nanos
    );
}
