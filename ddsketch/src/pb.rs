// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

// This file has been automatically generated from build.rs

/// A DDSketch is essentially a histogram that partitions the range of positive values into an infinite number of
/// indexed bins whose size grows exponentially. It keeps track of the number of values (or possibly floating-point
/// weights) added to each bin. Negative values are partitioned like positive values, symmetrically to zero.
/// The value zero as well as its close neighborhood that would be mapped to extreme bin indexes is mapped to a specific
/// counter.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DdSketch {
    /// The mapping between positive values and the bin indexes they belong to.
    #[prost(message, optional, tag = "1")]
    pub mapping: ::core::option::Option<IndexMapping>,
    /// The store for keeping track of positive values.
    #[prost(message, optional, tag = "2")]
    pub positive_values: ::core::option::Option<Store>,
    /// The store for keeping track of negative values. A negative value v is mapped using its positive opposite -v.
    #[prost(message, optional, tag = "3")]
    pub negative_values: ::core::option::Option<Store>,
    /// The count for the value zero and its close neighborhood (whose width depends on the mapping).
    #[prost(double, tag = "4")]
    pub zero_count: f64,
}
/// How to map positive values to the bins they belong to.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct IndexMapping {
    /// The gamma parameter of the mapping, such that bin index that a value v belongs to is roughly equal to
    /// log(v)/log(gamma).
    #[prost(double, tag = "1")]
    pub gamma: f64,
    /// An offset that can be used to shift all bin indexes.
    #[prost(double, tag = "2")]
    pub index_offset: f64,
    /// To speed up the computation of the index a value belongs to, the computation of the log may be approximated using
    /// the fact that the log to the base 2 of powers of 2 can be computed at a low cost from the binary representation of
    /// the input value. Other values can be approximated by interpolating between successive powers of 2 (linearly,
    /// quadratically or cubically).
    /// NONE means that the log is to be computed exactly (no interpolation).
    #[prost(enumeration = "index_mapping::Interpolation", tag = "3")]
    pub interpolation: i32,
}
/// Nested message and enum types in `IndexMapping`.
pub mod index_mapping {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum Interpolation {
        None = 0,
        Linear = 1,
        Quadratic = 2,
        Cubic = 3,
    }
    impl Interpolation {
        /// String value of the enum field names used in the ProtoBuf definition.
        ///
        /// The values are not transformed in any way and thus are considered stable
        /// (if the ProtoBuf definition does not change) and safe for programmatic use.
        pub fn as_str_name(&self) -> &'static str {
            match self {
                Interpolation::None => "NONE",
                Interpolation::Linear => "LINEAR",
                Interpolation::Quadratic => "QUADRATIC",
                Interpolation::Cubic => "CUBIC",
            }
        }
        /// Creates an enum from field names used in the ProtoBuf definition.
        pub fn from_str_name(value: &str) -> ::core::option::Option<Self> {
            match value {
                "NONE" => Some(Self::None),
                "LINEAR" => Some(Self::Linear),
                "QUADRATIC" => Some(Self::Quadratic),
                "CUBIC" => Some(Self::Cubic),
                _ => None,
            }
        }
    }
}
/// A Store maps bin indexes to their respective counts.
/// Counts can be encoded sparsely using binCounts, but also in a contiguous way using contiguousBinCounts and
/// contiguousBinIndexOffset. Given that non-empty bins are in practice usually contiguous or close to one another, the
/// latter contiguous encoding method is usually more efficient than the sparse one.
/// Both encoding methods can be used conjointly. If a bin appears in both the sparse and the contiguous encodings, its
/// count value is the sum of the counts in each encodings.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Store {
    /// The bin counts, encoded sparsely.
    #[prost(map = "sint32, double", tag = "1")]
    pub bin_counts: ::std::collections::HashMap<i32, f64>,
    /// The bin counts, encoded contiguously. The values of contiguousBinCounts are the counts for the bins of indexes
    /// o, o+1, o+2, etc., where o is contiguousBinIndexOffset.
    #[prost(double, repeated, tag = "2")]
    pub contiguous_bin_counts: ::prost::alloc::vec::Vec<f64>,
    #[prost(sint32, tag = "3")]
    pub contiguous_bin_index_offset: i32,
}
