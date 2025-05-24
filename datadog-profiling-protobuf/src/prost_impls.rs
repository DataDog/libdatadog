// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct Profile {
    #[prost(message, repeated, tag = "1")]
    pub sample_types: Vec<ValueType>,
    #[prost(message, repeated, tag = "2")]
    pub samples: Vec<Sample>,
    #[prost(message, repeated, tag = "3")]
    pub mappings: Vec<Mapping>,
    #[prost(message, repeated, tag = "4")]
    pub locations: Vec<Location>,
    #[prost(message, repeated, tag = "5")]
    pub functions: Vec<Function>,
    #[prost(string, repeated, tag = "6")]
    pub string_table: Vec<String>,
    #[prost(int64, tag = "7")]
    pub drop_frames: i64,
    #[prost(int64, tag = "8")]
    pub keep_frames: i64,
    #[prost(int64, tag = "9")]
    pub time_nanos: i64,
    #[prost(int64, tag = "10")]
    pub duration_nanos: i64,
    #[prost(message, optional, tag = "11")]
    pub period_type: Option<ValueType>,
    #[prost(int64, tag = "12")]
    pub period: i64,
    #[prost(int64, repeated, tag = "13")]
    pub comment: Vec<i64>,
    #[prost(int64, tag = "14")]
    pub default_sample_type: i64,
}

#[derive(Clone, Eq, Hash, PartialEq, PartialOrd, Ord, ::prost::Message)]
pub struct Sample {
    #[prost(uint64, repeated, tag = "1")]
    pub location_ids: Vec<u64>,
    #[prost(int64, repeated, tag = "2")]
    pub values: Vec<i64>,
    #[prost(message, repeated, tag = "3")]
    pub labels: Vec<Label>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, ::prost::Message)]
pub struct ValueType {
    #[prost(int64, tag = "1")]
    pub r#type: i64, // Index into string table
    #[prost(int64, tag = "2")]
    pub unit: i64, // Index into string table
}

#[derive(Clone, Eq, PartialEq, Hash, PartialOrd, Ord, ::prost::Message)]
pub struct Label {
    #[prost(int64, tag = "1")]
    pub key: i64, // Index into string table
    #[prost(int64, tag = "2")]
    pub str: i64, // Index into string table
    #[prost(int64, tag = "3")]
    pub num: i64,
    #[prost(int64, tag = "4")]
    pub num_unit: i64,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, ::prost::Message)]
pub struct Mapping {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(uint64, tag = "2")]
    pub memory_start: u64,
    #[prost(uint64, tag = "3")]
    pub memory_limit: u64,
    #[prost(uint64, tag = "4")]
    pub file_offset: u64,
    #[prost(int64, tag = "5")]
    pub filename: i64, // Index into string table
    #[prost(int64, tag = "6")]
    pub build_id: i64, // Index into string table
    #[prost(bool, tag = "7")]
    pub has_functions: bool,
    #[prost(bool, tag = "8")]
    pub has_filenames: bool,
    #[prost(bool, tag = "9")]
    pub has_line_numbers: bool,
    #[prost(bool, tag = "10")]
    pub has_inline_frames: bool,
}

#[derive(Clone, Eq, PartialEq, Hash, ::prost::Message)]
pub struct Location {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(uint64, tag = "2")]
    pub mapping_id: u64,
    #[prost(uint64, tag = "3")]
    pub address: u64,
    #[prost(message, repeated, tag = "4")]
    pub lines: Vec<Line>,
    #[prost(bool, tag = "5")]
    pub is_folded: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, ::prost::Message)]
pub struct Line {
    #[prost(uint64, tag = "1")]
    pub function_id: u64,
    #[prost(int64, tag = "2")]
    pub line: i64,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, ::prost::Message)]
pub struct Function {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(int64, tag = "2")]
    pub name: i64, // Index into string table
    #[prost(int64, tag = "3")]
    pub system_name: i64, // Index into string table
    #[prost(int64, tag = "4")]
    pub filename: i64, // Index into string table
}
