# datadog-profiling-otel

This module provides Rust bindings for the OpenTelemetry profiling protobuf definitions, generated using the `prost` library.

## Usage

### Basic Setup

Add this to your `Cargo.toml`:

```toml
[dependencies]
datadog-profiling-otel = "20.0.0"
```

### Creating Profile Data

```rust
use datadog_profiling_otel::*;

// Create a profiles dictionary
let mut profiles_dict = ProfilesDictionary::default();
profiles_dict.string_table.push("cpu".to_string());
profiles_dict.string_table.push("nanoseconds".to_string());

// Create a sample type
let sample_type = ValueType {
    type_strindex: 0, // "cpu"
    unit_strindex: 1, // "nanoseconds"
    aggregation_temporality: AggregationTemporality::Delta.into(),
};

// Create a profile
let mut profile = Profile::default();
profile.sample_type = Some(sample_type);
profile.time_nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos() as i64;

// Assemble the complete profiles data
let mut profiles_data = ProfilesData::default();
profiles_data.dictionary = Some(profiles_dict);

let mut scope_profiles = ScopeProfiles::default();
scope_profiles.profiles.push(profile);

let mut resource_profiles = ResourceProfiles::default();
resource_profiles.scope_profiles.push(scope_profiles);

profiles_data.resource_profiles.push(resource_profiles);
```

### Running Examples

```bash
# Run the basic usage example
cargo run --example basic_usage

# Run tests
cargo test

# Build
cargo build
```

## Module Structure

The generated code follows the OpenTelemetry protobuf structure:

- `ProfilesData`: Top-level container for all profile data
- `ResourceProfiles`: Profiles grouped by resource
- `ScopeProfiles`: Profiles grouped by instrumentation scope
- `Profile`: Individual profile with samples and metadata
- `ProfilesDictionary`: Shared data (strings, mappings, locations, etc.)
- `Sample`: Individual measurements with stack traces
- `Location`: Function locations in the call stack
- `Function`: Function information
- `Mapping`: Binary/library mapping information

## Dependencies

- `prost`: Protobuf implementation
- `prost-types`: Additional protobuf types
- `prost-build`: Build-time protobuf compilation

## License

Apache-2.0
