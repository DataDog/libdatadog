// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This file is a companion to the standard pprof protobuf definition.
//!
//! When `prost` serializes the standard `pprof` protobuf, it does a batch
//! operation:
//! 1. the entire `internal::Profile` is converted to `pprof::Profile`,
//! 2. which is then serialized into a byte-vector holding the protobuf,
//! 3. which is then compressed.
//!
//! This operation is memory inefficient:
//! 1. `pprof::Profile` does not deduplicate stack traces or labels, causing significant memory
//!    blow-up compared to `internal::Profile` for timelined traces.
//! 2. The `pprof` protobuf is highly compressible, particularly when timeline is enabled.  We are
//!    storing in memory a large buffer, that could have easily been compressed to a small one.
//! 3. Only at this point do we have a small in-memory buffer.
//!
//! If we stream the creation of the protobuf, we can avoid this memory blowup.
//! In particular, if each `pprof` Message (e.g. Sample, Location, etc) is
//! generated in a streaming fashion, and then immediately serialized and
//! compressed, we go directly from the efficient `internal` format to an
//! a compact compressed serialized format, without needing to ever create large
//! intermediate data-structures.
//!
//! We do this by taking advantage of the fact that the top level message in a
//! protobuf doesn't have a length header.  This means that the bits on the wire
//! are indistinguishable between serializing a single top-level message,
//! and serializing a series of top level message "slices" whose field indices
//! correspond to the indices in the unified top-level message.
//!
//! In particular, `pprof::Profile` fields are "sliced" into separate messages,
//! where `tag` number and type for each of these sliced messages matches the
//! corresponding field in the `pprof::Profile` message in `profile.proto`.
//!
//! Note that although many of these fields are of type `repeated` in the
//! underlying `pprof::Profile`, there is, (except for packed arrays of scalars,
//! which we don't use at this level), no difference in the byte representation
//! between
//! 1. repeatedly emitting a sliced message with a "required" field,
//! 2. repeatedly emitting a sliced message using a "repeated" field,
//! 3. Emitting once the message with the repeated field containing all values.
//!
//! In other words, we get the same bytes from "required" as "repeated", but
//! with fewer allocations (since we don't need a `Vec` for the elements).
//!
//! Note that it is important to make the field "required".  Pprof requires that
//! the "" string be the 0th element of the string table.  In a "repeated" field
//! default-valued messages are emitted; in an "optional" field, default-valued
//! messages are simply never emitted, and the parser just uses the default
//! value.  This drops the "" string, and messes up the string table.
//! Making all fields required stops this from happening and gives the desired
//! semantics.

use super::*;

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileSampleTypesEntry {
    #[prost(message, required, tag = "1")]
    pub sample_types_entry: ValueType,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileSamplesEntry {
    #[prost(message, required, tag = "2")]
    pub samples_entry: Sample,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileMappingsEntry {
    #[prost(message, required, tag = "3")]
    pub mappings_entry: Mapping,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileLocationsEntry {
    #[prost(message, required, tag = "4")]
    pub locations_entry: Location,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileFunctionsEntry {
    #[prost(message, required, tag = "5")]
    pub functions_entry: Function,
}

// These fields are not repeated so we can just make a combined struct for them.
#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileSimpler {
    #[prost(int64, tag = "9")]
    pub time_nanos: i64,
    #[prost(int64, tag = "10")]
    pub duration_nanos: i64,
    #[prost(message, optional, tag = "11")]
    pub period_type: Option<ValueType>,
    #[prost(int64, tag = "12")]
    pub period: i64,
}

impl From<ValueType> for ProfileSampleTypesEntry {
    fn from(item: ValueType) -> Self {
        Self {
            sample_types_entry: item,
        }
    }
}

impl From<Sample> for ProfileSamplesEntry {
    fn from(item: Sample) -> Self {
        Self {
            samples_entry: item,
        }
    }
}

impl From<Mapping> for ProfileMappingsEntry {
    fn from(item: Mapping) -> Self {
        Self {
            mappings_entry: item,
        }
    }
}

impl From<Location> for ProfileLocationsEntry {
    fn from(item: Location) -> Self {
        Self {
            locations_entry: item,
        }
    }
}

impl From<Function> for ProfileFunctionsEntry {
    fn from(item: Function) -> Self {
        Self {
            functions_entry: item,
        }
    }
}
