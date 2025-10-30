# libdd-profiling-protobuf

Protocol buffer definitions and implementations for the profiling pprof format.

## Overview

`libdd-profiling-protobuf` provides Rust types and serialization support for Google's pprof protocol buffer format, used for representing profiling data.

## Features

- **pprof Format**: Complete implementation of the pprof protobuf schema
- **Prost Integration**: Uses prost for protobuf serialization/deserialization
- **Type Safety**: Strongly-typed Rust representations of pprof structures
- **bolero Support**: Fuzz testing support for robust parsing
- **Serialization**: Efficient binary serialization for profile data

## pprof Types

The crate provides Rust types for all pprof entities:
- `Profile`: Top-level profile container
- `Sample`: Individual profiling samples
- `Location`: Code locations (addresses)
- `Function`: Function information
- `Mapping`: Binary/library mappings
- `ValueType`: Types of measured values
- `Label`: Key-value labels for samples

## Usage

```rust
use libdd_profiling_protobuf::prost_impls::Profile;

// Create a profile
let profile = Profile {
    sample_type: vec![/* value types */],
    sample: vec![/* samples */],
    mapping: vec![/* mappings */],
    location: vec![/* locations */],
    function: vec![/* functions */],
    ..Default::default()
};

// Serialize to bytes
let bytes = profile.encode_to_vec();
```

## Features Flags

- `prost_impls` (default): Enable prost serialization support
- `bolero`: Enable fuzz testing with bolero

## Specification

This crate implements the pprof format as specified by:
https://github.com/google/pprof/blob/main/proto/profile.proto

