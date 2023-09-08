// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

//! This file is a companion to the standard pprof protobuf definition.
//! `Profile` fields are "sliced" into separate messages, allowing them to be
//! serialized in a streamed manner.  This takes advantage of the fact that
//! the top level message in a protobuf doesn't have a length header, so the
//! bits on the wire are indistinguishable between serializing these sliced
//! messages, and serializing the top-level message.

use super::*;

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileSampleTypesEntry {
    #[prost(message, tag = "1")]
    pub sample_types_entry: Option<ValueType>,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileSamplesEntry {
    #[prost(message, tag = "2")]
    pub samples_entry: Option<Sample>,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileMappingsEntry {
    #[prost(message, tag = "3")]
    pub mappings_entry: Option<Mapping>,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileLocationsEntry {
    #[prost(message, tag = "4")]
    pub locations_entry: Option<Location>,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileFunctionsEntry {
    #[prost(message, tag = "5")]
    pub function_entry: Option<Function>,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileStringTableEntry {
    #[prost(string, repeated, tag = "6")]
    pub string_table_entry: Vec<String>,
}

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
