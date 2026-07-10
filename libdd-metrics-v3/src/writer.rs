// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! V3 columnar metrics writer.
//!
//! [`V3Writer`] accumulates metrics in columnar format with dictionary deduplication, then
//! produces [`V3EncodedData`] via [`V3Writer::into_columns`]. [`V3Writer::finalize`]
//! serializes that columnar data to protobuf wire format, using a hand-rolled encoder.

use alloc::{string::String, vec::Vec};

use crate::{
    constants::{
        DICT_NAME_STR_FIELD_NUMBER, DICT_ORIGIN_INFO_FIELD_NUMBER, DICT_RESOURCE_LEN_FIELD_NUMBER,
        DICT_RESOURCE_NAME_FIELD_NUMBER, DICT_RESOURCE_STR_FIELD_NUMBER,
        DICT_RESOURCE_TYPE_FIELD_NUMBER, DICT_SOURCE_TYPE_NAME_FIELD_NUMBER,
        DICT_TAGSETS_FIELD_NUMBER, DICT_TAGS_STR_FIELD_NUMBER, DICT_UNIT_STR_FIELD_NUMBER,
        INTERVALS_FIELD_NUMBER, NAMES_FIELD_NUMBER, NUM_POINTS_FIELD_NUMBER,
        ORIGIN_INFO_FIELD_NUMBER, RESOURCES_FIELD_NUMBER, SKETCH_BIN_CNTS_FIELD_NUMBER,
        SKETCH_BIN_KEYS_FIELD_NUMBER, SKETCH_NUM_BINS_FIELD_NUMBER, SOURCE_TYPE_NAME_FIELD_NUMBER,
        TAGS_FIELD_NUMBER, TIMESTAMPS_FIELD_NUMBER, TYPES_FIELD_NUMBER, UNIT_REFS_FIELD_NUMBER,
        VALS_FLOAT32_FIELD_NUMBER, VALS_FLOAT64_FIELD_NUMBER, VALS_SINT64_FIELD_NUMBER,
    },
    interner::Interner,
    types::{value_type_for_values, V3MetricType, V3ValueType},
};

pub const FLAG_NO_INDEX: u64 = 0x100;
pub const FLAG_HAS_UNIT: u64 = 0x200;

/// Bitmask for the base [`V3MetricType`] stored in bits 0-3 of a `types` column entry.
const METRIC_TYPE_MASK: u64 = 0x0F;

/// Errors returned by [`V3MetricBuilder`] methods when a caller violates one of their
/// preconditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3WriterError {
    /// [`add_point`](V3MetricBuilder::add_point) was called on a builder created with
    /// [`V3MetricType::Sketch`]; use [`add_sketch`](V3MetricBuilder::add_sketch) instead.
    PointOnSketchMetric,
    /// [`add_sketch`](V3MetricBuilder::add_sketch) was called on a builder created with a
    /// non-sketch metric type; use [`add_point`](V3MetricBuilder::add_point) instead.
    SketchOnNonSketchMetric,
    /// [`add_sketch`](V3MetricBuilder::add_sketch)'s `bin_keys` and `bin_counts` slices had
    /// different lengths.
    SketchBinLengthMismatch {
        /// Length of the `bin_keys` slice.
        bin_keys_len: usize,
        /// Length of the `bin_counts` slice.
        bin_counts_len: usize,
    },
}

impl core::fmt::Display for V3WriterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PointOnSketchMetric => {
                write!(
                    f,
                    "add_point called on a Sketch metric; use add_sketch instead"
                )
            }
            Self::SketchOnNonSketchMetric => {
                write!(
                    f,
                    "add_sketch called on a non-Sketch metric; use add_point instead"
                )
            }
            Self::SketchBinLengthMismatch {
                bin_keys_len,
                bin_counts_len,
            } => write!(
                f,
                "bin_keys length ({bin_keys_len}) does not match bin_counts length \
                 ({bin_counts_len})"
            ),
        }
    }
}

impl core::error::Error for V3WriterError {}

/// Encoded V3 payload data, prior to wire-format serialization.
///
/// This is the columnar representation produced by [`V3Writer::into_columns`], after delta
/// encoding but before any protobuf framing. Column names are given by [`crate::COLUMN_NAMES`]
/// and field numbers by the `*_FIELD_NUMBER` constants in this crate. Consumers with their own
/// Protocol Buffers implementation can serialize these columns directly instead of using
/// [`V3Writer::finalize`].
#[derive(Debug, Default)]
pub struct V3EncodedData {
    /// Dictionary of interned metric names, as varint-length-prefixed strings concatenated
    /// together.
    pub dict_name_bytes: Vec<u8>,
    /// Dictionary of interned tag strings, as varint-length-prefixed strings concatenated
    /// together.
    pub dict_tags_bytes: Vec<u8>,
    /// Dictionary of interned tag sets. Each entry is a count of tags followed by that many
    /// sorted, delta-encoded tag dictionary IDs.
    pub dict_tagsets: Vec<i64>,
    /// Dictionary of interned resource type/name strings, as varint-length-prefixed strings
    /// concatenated together.
    pub dict_resource_str_bytes: Vec<u8>,
    /// Number of (type, name) pairs in each interned resource set.
    pub dict_resource_len: Vec<i64>,
    /// Delta-encoded resource type dictionary IDs, grouped contiguously per interned resource set.
    pub dict_resource_type: Vec<i64>,
    /// Delta-encoded resource name dictionary IDs, grouped contiguously per interned resource set.
    pub dict_resource_name: Vec<i64>,
    /// Dictionary of interned source type name strings, as varint-length-prefixed strings
    /// concatenated together.
    pub dict_source_type_bytes: Vec<u8>,
    /// Dictionary of interned origin metadata, as flattened (product, category, service) triples.
    pub dict_origin_info: Vec<i32>,
    /// Dictionary of interned unit strings, as varint-length-prefixed strings concatenated
    /// together.
    pub dict_unit_bytes: Vec<u8>,

    /// Per-metric type and flags column.
    pub types: Vec<u64>,
    /// Per-metric delta-encoded name dictionary IDs.
    pub names: Vec<i64>,
    /// Per-metric delta-encoded tag set dictionary IDs.
    pub tags: Vec<i64>,
    /// Per-metric delta-encoded resource set dictionary IDs.
    pub resources: Vec<i64>,
    /// Per-metric interval, in seconds, used for rate metrics.
    pub intervals: Vec<u64>,
    /// Per-metric number of points.
    pub num_points: Vec<u64>,
    /// Per-metric delta-encoded source type name dictionary IDs.
    pub source_type_names: Vec<i64>,
    /// Per-metric delta-encoded origin metadata dictionary IDs.
    pub origin_infos: Vec<i64>,
    /// Per-metric delta-encoded unit dictionary IDs. Present only for metrics with `FLAG_HAS_UNIT`
    /// set.
    pub unit_refs: Vec<i64>,

    /// Delta-encoded point timestamps, across all metrics.
    pub timestamps: Vec<i64>,
    /// Point values stored as signed 64-bit integers.
    pub vals_sint64: Vec<i64>,
    /// Point values stored as 32-bit floats.
    pub vals_float32: Vec<f32>,
    /// Point values stored as 64-bit floats.
    pub vals_float64: Vec<f64>,

    /// Number of bins in each sketch.
    pub sketch_num_bins: Vec<u64>,
    /// Delta-encoded sketch bin keys, grouped contiguously per sketch.
    pub sketch_bin_keys: Vec<i32>,
    /// Sketch bin counts, grouped contiguously per sketch.
    pub sketch_bin_cnts: Vec<u32>,

    /// Telemetry produced while encoding the columns.
    pub value_encoding_stats: V3ValueEncodingStats,
}

/// Encoded V3 metrics payload with telemetry data.
pub struct V3EncodedMetrics {
    /// Serialized `MetricData` protobuf payload.
    pub payload: Vec<u8>,
    /// Telemetry produced while encoding the payload.
    pub stats: V3EncoderStats,
}

/// Telemetry data produced while encoding a V3 metrics payload.
pub struct V3EncoderStats {
    /// Counts of how many point values were compacted into each value column.
    pub value_encoding_stats: V3ValueEncodingStats,
    /// Raw bytes written for each present column, keyed by field number.
    pub columns: Vec<V3ColumnBytes>,
}

/// Counts of how many point values were encoded into each value column.
#[derive(Clone, Copy, Debug, Default)]
pub struct V3ValueEncodingStats {
    /// Number of point values that were zero and required no explicit storage.
    pub zero: u64,
    /// Number of point values stored as signed integers.
    pub sint64: u64,
    /// Number of point values stored as 32-bit floats.
    pub float32: u64,
    /// Number of point values stored as 64-bit floats.
    pub float64: u64,
}

/// Raw stream bytes for a single V3 column before protobuf field framing.
pub struct V3ColumnBytes {
    /// Protocol Buffers field number this column corresponds to.
    pub field_number: u32,
    /// Column contents, framed as an unwrapped (no field tag) protobuf value.
    pub bytes: Vec<u8>,
    /// Reserved for the compressed length of `bytes`; currently always `0`.
    pub compressed_len: usize,
}

/// V3 columnar metrics writer.
///
/// Accumulates metrics in columnar format with dictionary deduplication.
/// Call [`V3Writer::write`] for each metric, then [`V3Writer::finalize`] to finalize
/// and get the encoded data.
#[derive(Debug, Default)]
pub struct V3Writer {
    // Interners for dictionary deduplication
    name_interner: Interner<String>,
    tag_interner: Interner<String>,
    tagset_interner: Interner<Vec<i64>>,
    resource_str_interner: Interner<String>,
    resource_interner: Interner<Vec<(i64, i64)>>,
    source_type_interner: Interner<String>,
    origin_interner: Interner<(i32, i32, i32)>,
    unit_interner: Interner<String>,

    // Dictionary encoded bytes
    dict_name_bytes: Vec<u8>,
    dict_tags_bytes: Vec<u8>,
    dict_tagsets: Vec<i64>,
    dict_resource_str_bytes: Vec<u8>,
    dict_resource_len: Vec<i64>,
    dict_resource_type: Vec<i64>,
    dict_resource_name: Vec<i64>,
    dict_source_type_bytes: Vec<u8>,
    dict_origin_info: Vec<i32>,
    dict_unit_bytes: Vec<u8>,

    // Per-metric columns (one entry per metric, except conditional columns)
    types: Vec<u64>,
    names: Vec<i64>,
    tags: Vec<i64>,
    resources: Vec<i64>,
    intervals: Vec<u64>,
    num_points: Vec<u64>,
    source_type_names: Vec<i64>,
    origin_infos: Vec<i64>,
    unit_refs: Vec<i64>, // Present only for metrics with FLAG_HAS_UNIT set.

    // Point data
    timestamps: Vec<i64>,
    vals_sint64: Vec<i64>,
    vals_float32: Vec<f32>,
    vals_float64: Vec<f64>,

    // Sketch data
    sketch_num_bins: Vec<u64>,
    sketch_bin_keys: Vec<i32>,
    sketch_bin_cnts: Vec<u32>,

    // Scratch data
    tag_ids: Vec<i64>,
    resource_ids: Vec<(i64, i64)>,
    value_encoding_stats: V3ValueEncodingStats,
}

impl V3Writer {
    /// Creates a new V3 writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Begins writing a new metric.
    ///
    /// Returns a [`V3MetricBuilder`] that must be used to set the metric's
    /// properties and add points, then closed with [`V3MetricBuilder::close`].
    pub fn write(&mut self, metric_type: V3MetricType, name: &str) -> V3MetricBuilder<'_> {
        let name_id = self.intern_name(name);
        let metric_idx = self.types.len();
        let point_start_idx = self.vals_float64.len();
        let sint64_start_idx = self.vals_sint64.len();

        // Initialize the per-metric columns with default values
        self.types.push(metric_type.as_u64());
        self.names.push(name_id);
        self.tags.push(0);
        self.resources.push(0);
        self.intervals.push(0);
        self.num_points.push(0);
        self.source_type_names.push(0);
        self.origin_infos.push(0);

        V3MetricBuilder {
            writer: self,
            point_start_idx,
            sint64_start_idx,
            metric_idx,
            unit_ref_idx: None,
            closed: false,
        }
    }

    /// Finalizes the writer and returns the encoded columnar data.
    #[must_use]
    pub fn into_columns(mut self) -> V3EncodedData {
        // Delta encode all of the index arrays first.
        delta_encode(&mut self.names);
        delta_encode(&mut self.tags);
        delta_encode(&mut self.resources);
        delta_encode(&mut self.source_type_names);
        delta_encode(&mut self.origin_infos);
        delta_encode(&mut self.unit_refs);
        delta_encode(&mut self.timestamps);

        V3EncodedData {
            dict_name_bytes: self.dict_name_bytes,
            dict_tags_bytes: self.dict_tags_bytes,
            dict_tagsets: self.dict_tagsets,
            dict_resource_str_bytes: self.dict_resource_str_bytes,
            dict_resource_len: self.dict_resource_len,
            dict_resource_type: self.dict_resource_type,
            dict_resource_name: self.dict_resource_name,
            dict_source_type_bytes: self.dict_source_type_bytes,
            dict_origin_info: self.dict_origin_info,
            dict_unit_bytes: self.dict_unit_bytes,
            types: self.types,
            names: self.names,
            tags: self.tags,
            resources: self.resources,
            intervals: self.intervals,
            num_points: self.num_points,
            source_type_names: self.source_type_names,
            origin_infos: self.origin_infos,
            unit_refs: self.unit_refs,
            timestamps: self.timestamps,
            vals_sint64: self.vals_sint64,
            vals_float32: self.vals_float32,
            vals_float64: self.vals_float64,
            sketch_num_bins: self.sketch_num_bins,
            sketch_bin_keys: self.sketch_bin_keys,
            sketch_bin_cnts: self.sketch_bin_cnts,
            value_encoding_stats: self.value_encoding_stats,
        }
    }

    /// Finalizes the writer and serializes the data to the given output buffer.
    #[allow(clippy::too_many_lines)]
    pub fn finalize(self) -> V3EncodedMetrics {
        let data = self.into_columns();
        let mut output = Vec::new();
        let mut columns = Vec::new();

        // Dictionary fields (bytes - varint-length-prefixed strings concatenated)
        write_bytes_column(
            &mut output,
            &mut columns,
            DICT_NAME_STR_FIELD_NUMBER,
            &data.dict_name_bytes,
        );
        write_bytes_column(
            &mut output,
            &mut columns,
            DICT_TAGS_STR_FIELD_NUMBER,
            &data.dict_tags_bytes,
        );

        // Packed repeated fields for dictionaries
        write_packed_column(
            &mut output,
            &mut columns,
            DICT_TAGSETS_FIELD_NUMBER,
            &data.dict_tagsets,
            write_sint64_value,
        );

        write_bytes_column(
            &mut output,
            &mut columns,
            DICT_RESOURCE_STR_FIELD_NUMBER,
            &data.dict_resource_str_bytes,
        );

        write_packed_column(
            &mut output,
            &mut columns,
            DICT_RESOURCE_LEN_FIELD_NUMBER,
            &data.dict_resource_len,
            write_int64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            DICT_RESOURCE_TYPE_FIELD_NUMBER,
            &data.dict_resource_type,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            DICT_RESOURCE_NAME_FIELD_NUMBER,
            &data.dict_resource_name,
            write_sint64_value,
        );

        write_bytes_column(
            &mut output,
            &mut columns,
            DICT_SOURCE_TYPE_NAME_FIELD_NUMBER,
            &data.dict_source_type_bytes,
        );

        write_packed_column(
            &mut output,
            &mut columns,
            DICT_ORIGIN_INFO_FIELD_NUMBER,
            &data.dict_origin_info,
            write_int32_value,
        );
        write_bytes_column(
            &mut output,
            &mut columns,
            DICT_UNIT_STR_FIELD_NUMBER,
            &data.dict_unit_bytes,
        );

        // Per-metric columns
        write_packed_column(
            &mut output,
            &mut columns,
            TYPES_FIELD_NUMBER,
            &data.types,
            write_uint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            NAMES_FIELD_NUMBER,
            &data.names,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            TAGS_FIELD_NUMBER,
            &data.tags,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            RESOURCES_FIELD_NUMBER,
            &data.resources,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            INTERVALS_FIELD_NUMBER,
            &data.intervals,
            write_uint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            NUM_POINTS_FIELD_NUMBER,
            &data.num_points,
            write_uint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            SOURCE_TYPE_NAME_FIELD_NUMBER,
            &data.source_type_names,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            ORIGIN_INFO_FIELD_NUMBER,
            &data.origin_infos,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            UNIT_REFS_FIELD_NUMBER,
            &data.unit_refs,
            write_sint64_value,
        );

        // Point data
        write_packed_column(
            &mut output,
            &mut columns,
            TIMESTAMPS_FIELD_NUMBER,
            &data.timestamps,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            VALS_SINT64_FIELD_NUMBER,
            &data.vals_sint64,
            write_sint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            VALS_FLOAT32_FIELD_NUMBER,
            &data.vals_float32,
            write_f32_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            VALS_FLOAT64_FIELD_NUMBER,
            &data.vals_float64,
            write_f64_value,
        );

        // Sketch data
        write_packed_column(
            &mut output,
            &mut columns,
            SKETCH_NUM_BINS_FIELD_NUMBER,
            &data.sketch_num_bins,
            write_uint64_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            SKETCH_BIN_KEYS_FIELD_NUMBER,
            &data.sketch_bin_keys,
            write_sint32_value,
        );
        write_packed_column(
            &mut output,
            &mut columns,
            SKETCH_BIN_CNTS_FIELD_NUMBER,
            &data.sketch_bin_cnts,
            write_uint32_value,
        );

        V3EncodedMetrics {
            payload: output,
            stats: V3EncoderStats {
                value_encoding_stats: data.value_encoding_stats,
                columns,
            },
        }
    }

    fn intern_name(&mut self, name: &str) -> i64 {
        if name.is_empty() {
            return 0;
        }
        let (id, is_new) = self.name_interner.get_or_insert(name);
        if is_new {
            append_len_str(&mut self.dict_name_bytes, name);
        }
        id
    }

    fn intern_tag(&mut self, tag: &str) {
        if tag.is_empty() {
            self.tag_ids.push(0);
            return;
        }

        let (id, is_new) = self.tag_interner.get_or_insert(tag);
        if is_new {
            append_len_str(&mut self.dict_tags_bytes, tag);
        }
        self.tag_ids.push(id);
    }

    fn intern_tagset<I, S>(&mut self, tags: I) -> i64
    where
        I: Iterator<Item = S>,
        S: AsRef<str>,
    {
        self.tag_ids.clear();
        for tag in tags {
            self.intern_tag(tag.as_ref());
        }

        if self.tag_ids.is_empty() {
            return 0;
        }

        let (id, is_new) = self.tagset_interner.get_or_insert(&self.tag_ids);
        if is_new {
            self.encode_tagset();
        }
        id
    }

    fn encode_tagset(&mut self) {
        // Push the length. `tag_ids.len()` can never approach `i64::MAX`.
        #[allow(clippy::cast_possible_wrap)]
        self.dict_tagsets.push(self.tag_ids.len() as i64);

        let start = self.dict_tagsets.len();

        // Add all tag IDs
        self.dict_tagsets.extend_from_slice(&self.tag_ids);

        // Sort and delta-encode the tagset portion
        self.dict_tagsets[start..].sort_unstable();
        delta_encode(&mut self.dict_tagsets[start..]);
    }

    fn intern_resource_str(&mut self, s: &str) -> i64 {
        if s.is_empty() {
            return 0;
        }
        let (id, is_new) = self.resource_str_interner.get_or_insert(s);
        if is_new {
            append_len_str(&mut self.dict_resource_str_bytes, s);
        }
        id
    }

    fn intern_resources(&mut self, resources: &[(&str, &str)]) -> i64 {
        self.resource_ids.clear();
        for (resource_type, resource_name) in resources {
            let type_id = self.intern_resource_str(resource_type);
            let name_id = self.intern_resource_str(resource_name);
            self.resource_ids.push((type_id, name_id));
        }

        if self.resource_ids.is_empty() {
            return 0;
        }

        let (id, is_new) = self.resource_interner.get_or_insert(&self.resource_ids);
        if is_new {
            self.encode_resources();
        }
        id
    }

    fn encode_resources(&mut self) {
        // `resource_ids.len()` can never approach `i64::MAX`.
        #[allow(clippy::cast_possible_wrap)]
        self.dict_resource_len.push(self.resource_ids.len() as i64);

        let type_start = self.dict_resource_type.len();
        let name_start = self.dict_resource_name.len();

        for (type_id, name_id) in &self.resource_ids {
            self.dict_resource_type.push(*type_id);
            self.dict_resource_name.push(*name_id);
        }

        delta_encode(&mut self.dict_resource_type[type_start..]);
        delta_encode(&mut self.dict_resource_name[name_start..]);
    }

    fn intern_source_type(&mut self, s: &str) -> i64 {
        if s.is_empty() {
            return 0;
        }
        let (id, is_new) = self.source_type_interner.get_or_insert(s);
        if is_new {
            append_len_str(&mut self.dict_source_type_bytes, s);
        }
        id
    }

    fn intern_origin(&mut self, product: i32, category: i32, service: i32) -> i64 {
        if product == 0 && category == 0 && service == 0 {
            return 0;
        }

        let (id, is_new) = self
            .origin_interner
            .get_or_insert(&(product, category, service));
        if is_new {
            self.dict_origin_info.push(product);
            self.dict_origin_info.push(category);
            self.dict_origin_info.push(service);
        }
        id
    }

    fn intern_unit(&mut self, unit: &str) -> i64 {
        if unit.is_empty() {
            return 0;
        }
        let (id, is_new) = self.unit_interner.get_or_insert(unit);
        if is_new {
            append_len_str(&mut self.dict_unit_bytes, unit);
        }
        id
    }
}

/// Builder for a single metric within a V3 payload.
///
/// Use the setter methods to configure the metric, add points with [`add_point`](Self::add_point),
/// then call [`close`](Self::close) to finalize. If dropped without calling `close`, the metric
/// is finalized automatically.
pub struct V3MetricBuilder<'a> {
    writer: &'a mut V3Writer,
    point_start_idx: usize,
    sint64_start_idx: usize,
    metric_idx: usize,
    unit_ref_idx: Option<usize>,
    closed: bool,
}

impl Drop for V3MetricBuilder<'_> {
    /// Finalizes the metric if [`close`](Self::close) was never called.
    fn drop(&mut self) {
        if !self.closed {
            self.compact_values();
        }
    }
}

impl V3MetricBuilder<'_> {
    /// Sets the tags for this metric.
    ///
    /// Tags should be in "key:value" format.
    pub fn set_tags<I, S>(&mut self, tags: I)
    where
        I: Iterator<Item = S>,
        S: AsRef<str>,
    {
        let tagset_id = self.writer.intern_tagset(tags);
        self.writer.tags[self.metric_idx] = tagset_id;
    }

    /// Sets the resources for this metric.
    ///
    /// Resources are (type, name) pairs, for example, (`host`, `server1`).
    pub fn set_resources(&mut self, resources: &[(&str, &str)]) {
        let res_id = self.writer.intern_resources(resources);
        self.writer.resources[self.metric_idx] = res_id;
    }

    /// Sets the interval for this metric (used for rate metrics).
    pub fn set_interval(&mut self, interval: u64) {
        self.writer.intervals[self.metric_idx] = interval;
    }

    /// Sets the source type name for this metric.
    pub fn set_source_type(&mut self, source_type: &str) {
        if source_type.is_empty() {
            self.writer.source_type_names[self.metric_idx] = 0;
            return;
        }
        let id = self.writer.intern_source_type(source_type);
        self.writer.source_type_names[self.metric_idx] = id;
    }

    /// Sets the origin metadata for this metric.
    pub fn set_origin(&mut self, product: u32, category: u32, service: u32, no_index: bool) {
        let id = self.writer.intern_origin(
            product.cast_signed(),
            category.cast_signed(),
            service.cast_signed(),
        );
        self.writer.origin_infos[self.metric_idx] = id;
        if no_index {
            self.writer.types[self.metric_idx] |= FLAG_NO_INDEX;
        } else {
            self.writer.types[self.metric_idx] &= !FLAG_NO_INDEX;
        }
    }

    /// Sets the unit for this metric.
    pub fn set_unit(&mut self, unit: &str) {
        if unit.is_empty() {
            self.writer.types[self.metric_idx] &= !FLAG_HAS_UNIT;
            if let Some(unit_ref_idx) = self.unit_ref_idx.take() {
                self.writer.unit_refs.remove(unit_ref_idx);
            }
            return;
        }

        let id = self.writer.intern_unit(unit);
        if let Some(unit_ref_idx) = self.unit_ref_idx {
            self.writer.unit_refs[unit_ref_idx] = id;
        } else {
            self.unit_ref_idx = Some(self.writer.unit_refs.len());
            self.writer.unit_refs.push(id);
        }
        self.writer.types[self.metric_idx] |= FLAG_HAS_UNIT;
    }

    /// Adds a data point to this metric.
    ///
    /// # Errors
    ///
    /// Returns [`V3WriterError::PointOnSketchMetric`] if this builder was created with
    /// [`V3MetricType::Sketch`]; use [`add_sketch`](Self::add_sketch) for sketch metrics instead.
    pub fn add_point(&mut self, timestamp: i64, value: f64) -> Result<(), V3WriterError> {
        if (self.writer.types[self.metric_idx] & METRIC_TYPE_MASK) == V3MetricType::Sketch as u64 {
            return Err(V3WriterError::PointOnSketchMetric);
        }

        self.writer.timestamps.push(timestamp);
        self.writer.vals_float64.push(value);
        self.writer.num_points[self.metric_idx] += 1;
        Ok(())
    }

    /// Adds sketch data for a distribution metric.
    ///
    /// For sketches, the summary values (count, sum, min, max) are stored as points,
    /// and the bin keys/counts are stored separately.
    ///
    /// # Errors
    ///
    /// Returns [`V3WriterError::SketchOnNonSketchMetric`] if this builder was not created with
    /// [`V3MetricType::Sketch`], or [`V3WriterError::SketchBinLengthMismatch`] if `bin_keys` and
    /// `bin_counts` have different lengths.
    #[allow(clippy::too_many_arguments)]
    pub fn add_sketch(
        &mut self,
        timestamp: i64,
        count: i64,
        sum: f64,
        min: f64,
        max: f64,
        bin_keys: &[i32],
        bin_counts: &[u32],
    ) -> Result<(), V3WriterError> {
        if (self.writer.types[self.metric_idx] & METRIC_TYPE_MASK) != V3MetricType::Sketch as u64 {
            return Err(V3WriterError::SketchOnNonSketchMetric);
        }
        if bin_keys.len() != bin_counts.len() {
            return Err(V3WriterError::SketchBinLengthMismatch {
                bin_keys_len: bin_keys.len(),
                bin_counts_len: bin_counts.len(),
            });
        }

        self.writer.timestamps.push(timestamp);

        // Count goes in sint64, sum/min/max go in float64
        self.writer.vals_sint64.push(count);
        self.writer.vals_float64.push(sum);
        self.writer.vals_float64.push(min);
        self.writer.vals_float64.push(max);

        // Store bin data
        self.writer.sketch_num_bins.push(bin_keys.len() as u64);

        let key_start = self.writer.sketch_bin_keys.len();
        self.writer.sketch_bin_keys.extend_from_slice(bin_keys);
        self.writer.sketch_bin_cnts.extend_from_slice(bin_counts);

        // Delta-encode this sketch's bin keys
        delta_encode_i32(&mut self.writer.sketch_bin_keys[key_start..]);

        self.writer.num_points[self.metric_idx] += 1;
        Ok(())
    }

    /// Finalizes this metric.
    pub fn close(mut self) {
        // Compacts the point values to use the smallest representation that can hold
        // all values without loss.
        self.compact_values();
        self.closed = true;
    }

    #[allow(clippy::cast_possible_truncation)]
    fn compact_values(&mut self) {
        let count = self.writer.num_points[self.metric_idx] as usize;
        if count == 0 {
            return;
        }

        let start = self.point_start_idx;
        let end = self.writer.vals_float64.len();

        // Determine the best value type for all points in this metric.
        let val_ty = value_type_for_values(self.writer.vals_float64[start..end].iter().copied());
        let is_sketch =
            (self.writer.types[self.metric_idx] & METRIC_TYPE_MASK) == V3MetricType::Sketch as u64;
        let float_values_len = end - start;
        if is_sketch {
            // Sketches always carry one integer count per point in addition to sum/min/max values.
            self.writer.value_encoding_stats.sint64 += count as u64;
        }

        // Update the type field
        self.writer.types[self.metric_idx] |= val_ty.as_u64();

        // Convert values to the appropriate storage
        match val_ty {
            V3ValueType::Zero => {
                self.writer.value_encoding_stats.zero += float_values_len as u64;
                // Values are all zero, don't store anything
                self.writer.vals_float64.truncate(start);
            }
            V3ValueType::Sint64 => {
                self.writer.value_encoding_stats.sint64 += float_values_len as u64;
                if is_sketch {
                    // For sketches, vals_sint64 already has one count per point (pushed by
                    // add_sketch), and vals_float64 has 3 values per point
                    // (sum, min, max). When compacting to Sint64, we need to
                    // interleave them as: sum, min, max, cnt per point.
                    let counts: Vec<i64> =
                        self.writer.vals_sint64[self.sint64_start_idx..].to_vec();
                    self.writer.vals_sint64.truncate(self.sint64_start_idx);
                    for (i, cnt) in counts.into_iter().enumerate() {
                        let f_off = start + i * 3;
                        self.writer
                            .vals_sint64
                            .push(self.writer.vals_float64[f_off] as i64);
                        self.writer
                            .vals_sint64
                            .push(self.writer.vals_float64[f_off + 1] as i64);
                        self.writer
                            .vals_sint64
                            .push(self.writer.vals_float64[f_off + 2] as i64);
                        self.writer.vals_sint64.push(cnt);
                    }
                } else {
                    for i in start..end {
                        self.writer
                            .vals_sint64
                            .push(self.writer.vals_float64[i] as i64);
                    }
                }
                self.writer.vals_float64.truncate(start);
            }
            V3ValueType::Float32 => {
                self.writer.value_encoding_stats.float32 += float_values_len as u64;
                for i in start..end {
                    self.writer
                        .vals_float32
                        .push(self.writer.vals_float64[i] as f32);
                }
                self.writer.vals_float64.truncate(start);
            }
            V3ValueType::Float64 => {
                self.writer.value_encoding_stats.float64 += float_values_len as u64;
                // Already stored in vals_float64, keep them
            }
        }
    }
}

/// Protobuf wire type for length-delimited fields (bytes, strings, packed repeated fields).
const WIRE_LEN: u32 = 2;

/// Writes a raw protobuf varint (LEB128, 7 bits per byte).
fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

/// Writes a protobuf field tag: `(field_number << 3) | wire_type`, as a varint.
fn write_tag(buf: &mut Vec<u8>, field_number: u32, wire_type: u32) {
    write_varint(buf, (u64::from(field_number) << 3) | u64::from(wire_type));
}

/// Bit-reinterprets (not arithmetically converts) `v` as unsigned; this is the standard protobuf
/// zigzag transform, not a lossy narrowing.
const fn zigzag64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)).cast_unsigned()
}

/// See [`zigzag64`].
const fn zigzag32(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)).cast_unsigned()
}

// Scalar value encoders, one per protobuf field type used by the V3 payload. Each writes a
// single value with no tag or length prefix, matching the layout of a packed repeated field's
// payload (and reused as-is for `V3ColumnBytes::bytes`, which is exactly that payload).

fn write_uint64_value(buf: &mut Vec<u8>, v: u64) {
    write_varint(buf, v);
}

fn write_sint64_value(buf: &mut Vec<u8>, v: i64) {
    write_varint(buf, zigzag64(v));
}

/// Protobuf's `int64` wire type is a plain varint of the value's two's-complement bit pattern
/// (inefficient for negative numbers, but that's the wire format).
fn write_int64_value(buf: &mut Vec<u8>, v: i64) {
    write_varint(buf, v.cast_unsigned());
}

/// See [`write_int64_value`]; `int32` is sign-extended to 64 bits before the same treatment.
fn write_int32_value(buf: &mut Vec<u8>, v: i32) {
    write_varint(buf, i64::from(v).cast_unsigned());
}

fn write_sint32_value(buf: &mut Vec<u8>, v: i32) {
    write_varint(buf, u64::from(zigzag32(v)));
}

fn write_uint32_value(buf: &mut Vec<u8>, v: u32) {
    write_varint(buf, u64::from(v));
}

fn write_f32_value(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_f64_value(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Writes a `bytes` field (tag + varint length + data). No-op if `bytes` is empty.
fn write_bytes_column(
    output: &mut Vec<u8>,
    columns: &mut Vec<V3ColumnBytes>,
    field_number: u32,
    bytes: &[u8],
) {
    if bytes.is_empty() {
        return;
    }

    write_tag(output, field_number, WIRE_LEN);
    write_varint(output, bytes.len() as u64);
    output.extend_from_slice(bytes);

    columns.push(V3ColumnBytes {
        field_number,
        bytes: bytes.to_vec(),
        compressed_len: 0,
    });
}

/// Writes a packed repeated field (tag + varint byte-length + concatenated encoded values).
/// No-op if `values` is empty.
fn write_packed_column<T: Copy>(
    output: &mut Vec<u8>,
    columns: &mut Vec<V3ColumnBytes>,
    field_number: u32,
    values: &[T],
    encode_one: fn(&mut Vec<u8>, T),
) {
    if values.is_empty() {
        return;
    }

    let mut raw = Vec::new();
    for &v in values {
        encode_one(&mut raw, v);
    }

    write_tag(output, field_number, WIRE_LEN);
    write_varint(output, raw.len() as u64);
    output.extend_from_slice(&raw);

    columns.push(V3ColumnBytes {
        field_number,
        bytes: raw,
        compressed_len: 0,
    });
}

fn append_len_str(dst: &mut Vec<u8>, s: &str) {
    let mut len = s.len() as u64;
    loop {
        let mut byte = (len & 0x7F) as u8;
        len >>= 7;
        if len != 0 {
            byte |= 0x80;
        }
        dst.push(byte);
        if len == 0 {
            break;
        }
    }
    dst.extend_from_slice(s.as_bytes());
}

fn delta_encode(s: &mut [i64]) {
    if s.len() < 2 {
        return;
    }
    for i in (1..s.len()).rev() {
        s[i] -= s[i - 1];
    }
}

fn delta_encode_i32(s: &mut [i32]) {
    if s.len() < 2 {
        return;
    }
    for i in (1..s.len()).rev() {
        s[i] -= s[i - 1];
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_lossless)]
mod tests {
    use super::*;

    #[test]
    fn test_delta_encode() {
        let mut data = vec![100, 110, 130, 145];
        delta_encode(&mut data);
        assert_eq!(data, vec![100, 10, 20, 15]);
    }

    #[test]
    fn test_delta_encode_empty() {
        let mut data: Vec<i64> = vec![];
        delta_encode(&mut data);
        assert!(data.is_empty());
    }

    #[test]
    fn test_delta_encode_single() {
        let mut data = vec![42];
        delta_encode(&mut data);
        assert_eq!(data, vec![42]);
    }

    #[test]
    fn test_append_len_str() {
        let mut buf = Vec::new();
        append_len_str(&mut buf, "hello");
        // Length 5 = 0x05, then "hello"
        assert_eq!(buf, vec![5, b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn test_varint_encoding() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 0);
        assert_eq!(buf, [0x00]);
        buf.clear();
        write_varint(&mut buf, 127);
        assert_eq!(buf, [0x7f]);
        buf.clear();
        write_varint(&mut buf, 128);
        assert_eq!(buf, [0x80, 0x01]);
        buf.clear();
        write_varint(&mut buf, 300);
        assert_eq!(buf, [0xac, 0x02]);
    }

    #[test]
    fn test_zigzag64() {
        assert_eq!(zigzag64(0), 0);
        assert_eq!(zigzag64(-1), 1);
        assert_eq!(zigzag64(1), 2);
        assert_eq!(zigzag64(-2), 3);
        assert_eq!(zigzag64(2147483647), 4294967294);
        assert_eq!(zigzag64(-2147483648), 4294967295);
    }

    #[test]
    fn test_zigzag32() {
        assert_eq!(zigzag32(0), 0);
        assert_eq!(zigzag32(-1), 1);
        assert_eq!(zigzag32(1), 2);
        assert_eq!(zigzag32(-2), 3);
    }

    #[test]
    fn test_write_f32_value_little_endian() {
        let mut buf = Vec::new();
        write_f32_value(&mut buf, 1.0);
        // 1.0f32 = 0x3f800000; LE bytes = [0x00, 0x00, 0x80, 0x3f]
        assert_eq!(buf, [0x00, 0x00, 0x80, 0x3f]);
    }

    #[test]
    fn test_write_f64_value_little_endian() {
        let mut buf = Vec::new();
        write_f64_value(&mut buf, 1.0);
        assert_eq!(buf, 1.0f64.to_le_bytes());
    }

    #[test]
    fn test_writer_basic() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "test.metric");
            metric.set_tags(["env:prod", "service:web"].iter().copied());
            metric.add_point(1000, 42.0).unwrap();
            metric.add_point(1010, 43.5).unwrap();
            metric.close();
        }

        let data = writer.into_columns();

        assert_eq!(data.types.len(), 1);
        assert_eq!(data.names.len(), 1);
        assert_eq!(data.timestamps.len(), 2);
    }

    #[test]
    fn test_writer_unit() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "has.unit");
            metric.set_unit("millisecond");
            metric.add_point(1000, 42.0).unwrap();
            metric.close();
        }
        {
            let mut metric = writer.write(V3MetricType::Gauge, "no.unit");
            metric.add_point(1000, 43.0).unwrap();
            metric.close();
        }
        {
            let mut metric = writer.write(V3MetricType::Gauge, "same.unit");
            metric.set_unit("millisecond");
            metric.add_point(1000, 44.0).unwrap();
            metric.close();
        }

        let data = writer.into_columns();

        assert_eq!(data.unit_refs, vec![1, 0]);
        assert_eq!(data.dict_unit_bytes, b"\x0bmillisecond");
        assert_eq!(data.types[0] & FLAG_HAS_UNIT, FLAG_HAS_UNIT);
        assert_eq!(data.types[1] & FLAG_HAS_UNIT, 0);
        assert_eq!(data.types[2] & FLAG_HAS_UNIT, FLAG_HAS_UNIT);
    }

    #[test]
    fn test_writer_multiple_metrics() {
        let mut writer = V3Writer::new();

        {
            let mut m1 = writer.write(V3MetricType::Count, "metric1");
            m1.add_point(1000, 10.0).unwrap();
            m1.close();
        }

        {
            let mut m2 = writer.write(V3MetricType::Rate, "metric2");
            m2.set_interval(60);
            m2.add_point(2000, 20.0).unwrap();
            m2.close();
        }

        let data = writer.into_columns();

        assert_eq!(data.types.len(), 2);
        assert_eq!(data.names.len(), 2);
        assert_eq!(data.intervals[0], 0);
        // Second metric's interval won't be 60 directly since names is delta-encoded,
        // but we can verify the structure is correct
    }

    #[test]
    fn test_value_compaction_zero() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "zero.metric");
            metric.add_point(1000, 0.0).unwrap();
            metric.add_point(2000, 0.0).unwrap();
            metric.close();
        }

        let data = writer.into_columns();

        // Values should be compacted - zero values don't need storage
        assert!(data.vals_float64.is_empty());
        assert!(data.vals_sint64.is_empty());
        assert!(data.vals_float32.is_empty());
    }

    #[test]
    fn test_value_compaction_int() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Count, "int.metric");
            metric.add_point(1000, 100.0).unwrap();
            metric.add_point(2000, 200.0).unwrap();
            metric.close();
        }

        let data = writer.into_columns();

        // Integer values should be stored in sint64
        assert!(data.vals_float64.is_empty());
        assert_eq!(data.vals_sint64, vec![100, 200]);
        assert!(data.vals_float32.is_empty());
    }

    #[test]
    fn test_serialize_empty() {
        let writer = V3Writer::new();
        let encoded = writer.finalize();
        assert!(encoded.payload.is_empty());
    }

    #[test]
    fn test_value_compaction_large_int_plus_float32() {
        // Regression test: a large integer (> 2^24) mixed with a fractional
        // float32 value must use Float64, not Float32, to avoid precision loss.
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "mixed.metric");
            metric.add_point(1000, (1i64 << 30) as f64).unwrap(); // large int, doesn't fit in f32
            metric.add_point(2000, 1.5).unwrap(); // fractional, fits in f32
            metric.close();
        }

        let data = writer.into_columns();

        // Must be stored in float64, not float32
        assert!(
            data.vals_float32.is_empty(),
            "large int should not be stored as float32"
        );
        assert_eq!(data.vals_float64, vec![(1i64 << 30) as f64, 1.5]);
        assert!(data.vals_sint64.is_empty());
    }

    #[test]
    fn test_value_compaction_small_int_plus_float32() {
        // Small integers (|v| <= 2^24) mixed with float32 values should
        // compact to Float32, since small ints fit losslessly in f32.
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "small.mixed");
            metric.add_point(1000, 100.0).unwrap();
            metric.add_point(2000, 1.5).unwrap();
            metric.close();
        }

        let data = writer.into_columns();

        assert!(data.vals_float64.is_empty());
        assert_eq!(data.vals_float32, vec![100.0, 1.5]);
        assert!(data.vals_sint64.is_empty());
    }

    #[test]
    fn test_serialize_basic_metric() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "test.metric");
            metric.add_point(1000, 42.0).unwrap();
            metric.close();
        }

        let encoded = writer.finalize();

        // Should produce non-empty output
        assert!(!encoded.payload.is_empty());
        assert_eq!(encoded.stats.value_encoding_stats.sint64, 1);
    }

    #[test]
    fn test_column_stats_for_bytes_column_use_raw_column_stream() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "test.metric");
            metric.add_point(1000, 42.0).unwrap();
            metric.close();
        }

        let encoded = writer.finalize();
        let name_column = encoded
            .stats
            .columns
            .iter()
            .find(|column| column.field_number == DICT_NAME_STR_FIELD_NUMBER)
            .expect("name dictionary column should be present");

        let mut expected = Vec::new();
        append_len_str(&mut expected, "test.metric");
        assert_eq!(name_column.bytes, expected);
    }

    #[test]
    fn test_column_stats_for_packed_column_use_raw_column_stream() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "test.metric");
            metric.add_point(1000, 42.0).unwrap();
            metric.close();
        }

        let encoded = writer.finalize();
        let timestamps_column = encoded
            .stats
            .columns
            .iter()
            .find(|column| column.field_number == TIMESTAMPS_FIELD_NUMBER)
            .expect("timestamp column should be present");

        let mut expected = Vec::new();
        write_sint64_value(&mut expected, 1000);
        assert_eq!(timestamps_column.bytes, expected);
    }

    #[test]
    fn test_column_stats_do_not_include_absent_columns() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "test.metric");
            metric.add_point(1000, 42.0).unwrap();
            metric.close();
        }

        let encoded = writer.finalize();
        assert!(!encoded
            .stats
            .columns
            .iter()
            .any(|column| column.field_number == UNIT_REFS_FIELD_NUMBER));
    }

    #[test]
    fn test_set_origin_clears_no_index_flag_when_reset() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "origin.metric");
            metric.set_origin(1, 2, 3, true);
            metric.set_origin(1, 2, 3, false);
            metric.add_point(1000, 1.0).unwrap();
            metric.close();
        }

        let data = writer.into_columns();
        assert_eq!(data.types[0] & FLAG_NO_INDEX, 0);
    }

    #[test]
    fn test_builder_finalizes_on_drop_without_close() {
        let mut writer = V3Writer::new();

        {
            let mut metric = writer.write(V3MetricType::Gauge, "dropped.metric");
            metric.add_point(1000, 42.0).unwrap();
            // Deliberately not calling `close`; `Drop` must finalize the metric anyway.
        }

        let data = writer.into_columns();
        assert_eq!(data.vals_sint64, vec![42]);
        assert!(data.vals_float64.is_empty());
        assert_eq!(data.types[0] & 0x30, V3ValueType::Sint64.as_u64());
    }

    #[test]
    fn test_add_point_rejected_on_sketch_metric() {
        let mut writer = V3Writer::new();
        let mut metric = writer.write(V3MetricType::Sketch, "wrong.method");
        assert_eq!(
            metric.add_point(1000, 1.0),
            Err(V3WriterError::PointOnSketchMetric)
        );
    }

    #[test]
    fn test_add_sketch_rejected_on_non_sketch_metric() {
        let mut writer = V3Writer::new();
        let mut metric = writer.write(V3MetricType::Gauge, "wrong.method");
        assert_eq!(
            metric.add_sketch(1000, 1, 1.0, 1.0, 1.0, &[0], &[1]),
            Err(V3WriterError::SketchOnNonSketchMetric)
        );
    }

    #[test]
    fn test_add_sketch_rejected_on_bin_length_mismatch() {
        let mut writer = V3Writer::new();
        let mut metric = writer.write(V3MetricType::Sketch, "mismatched.bins");
        assert_eq!(
            metric.add_sketch(1000, 1, 1.0, 1.0, 1.0, &[0, 1], &[1]),
            Err(V3WriterError::SketchBinLengthMismatch {
                bin_keys_len: 2,
                bin_counts_len: 1,
            })
        );
    }
}
