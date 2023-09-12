// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

//! This file is a companion to the standard pprof protobuf definition.
//! `Profile` fields are "sliced" into separate messages, allowing them to be
//! serialized in a streamed manner.  This takes advantage of the fact that
//! the top level message in a protobuf doesn't have a length header, so the
//! bits on the wire are indistinguishable between serializing these sliced
//! messages, and serializing the top-level message.
//!
//! The `tag` number and type for each of these sliced messages matches the
//! corresponding field in the `Profile` message in `profile.proto`.
//!
//! Note that although many of these fields are of type `repeated` in the
//! underlying `pprof::Profile`, there is, (except for packed arrays of scalars,
//! which we don't use at this level), no difference in the byte representation
//! between using a "required" field, and using a "repeated" field with one
//! element.  
//! In other words, we get the same bytes from "required" as "repeated", but
//! with fewer allocations (since we don't need a `Vec` for the single element).

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
    pub function_entry: Function,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileStringTableEntry {
    // profile.proto requires that 'string_table[0] must always be "".'
    // Writing "" to a protobuf is a no-op, unless the field is "repeated".
    // Making this field repeated (and hence take Vec<String>) ensures that the
    // initial "" will be correctly serialized.
    #[prost(string, required, tag = "6")]
    pub string_table_entry: String,
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
            function_entry: item,
        }
    }
}

impl From<String> for ProfileStringTableEntry {
    fn from(item: String) -> Self {
        Self {
            string_table_entry: item,
        }
    }
}
