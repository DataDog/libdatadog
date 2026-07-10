// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! V3 columnar protobuf codec for Datadog metrics.
//!
//! This crate implements the V3 format for Datadog metrics payloads which uses
//! a columnar layout with dictionary-based string deduplication for efficient encoding.
//!
//! [`V3Writer`] accumulates metrics one at a time via [`V3Writer::write`], then
//! [`V3Writer::into_columns`] produces the encoded columns. [`V3Writer::finalize`]
//! serializes those columns into a protobuf payload.
//!
//! Consumers with their own Protocol Buffers implementation can instead serialize [`V3EncodedData`]
//! directly using the `*_FIELD_NUMBER` constants.

#![cfg_attr(not(test), no_std)]
#![deny(missing_docs)]
#![deny(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

extern crate alloc;

mod constants;
mod interner;
mod types;
mod writer;

pub use constants::{
    COLUMN_NAMES, DICT_NAME_STR_FIELD_NUMBER, DICT_ORIGIN_INFO_FIELD_NUMBER,
    DICT_RESOURCE_LEN_FIELD_NUMBER, DICT_RESOURCE_NAME_FIELD_NUMBER,
    DICT_RESOURCE_STR_FIELD_NUMBER, DICT_RESOURCE_TYPE_FIELD_NUMBER,
    DICT_SOURCE_TYPE_NAME_FIELD_NUMBER, DICT_TAGSETS_FIELD_NUMBER, DICT_TAGS_STR_FIELD_NUMBER,
    DICT_UNIT_STR_FIELD_NUMBER, INTERVALS_FIELD_NUMBER, NAMES_FIELD_NUMBER,
    NUM_POINTS_FIELD_NUMBER, ORIGIN_INFO_FIELD_NUMBER, RESOURCES_FIELD_NUMBER,
    SKETCH_BIN_CNTS_FIELD_NUMBER, SKETCH_BIN_KEYS_FIELD_NUMBER, SKETCH_NUM_BINS_FIELD_NUMBER,
    SOURCE_TYPE_NAME_FIELD_NUMBER, TAGS_FIELD_NUMBER, TIMESTAMPS_FIELD_NUMBER, TYPES_FIELD_NUMBER,
    UNIT_REFS_FIELD_NUMBER, VALS_FLOAT32_FIELD_NUMBER, VALS_FLOAT64_FIELD_NUMBER,
    VALS_SINT64_FIELD_NUMBER,
};
pub use types::V3MetricType;
pub use writer::{
    V3EncodedData, V3EncodedMetrics, V3EncoderStats, V3MetricBuilder, V3ValueEncodingStats,
    V3Writer, V3WriterError,
};
