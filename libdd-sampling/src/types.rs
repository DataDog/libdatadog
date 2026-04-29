// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Type definitions and traits for sampling

use std::borrow::Cow;

/// A trait for converting trace IDs to a numeric representation.
///
/// Provides a common interface for converting trace IDs from different tracing systems
/// into a 128-bit unsigned integer for use in hash-based operations.
///
/// # Examples
///
/// ```
/// use libdd_sampling::TraceIdLike;
///
/// #[derive(Clone, PartialEq, Eq)]
/// struct MyTraceId(u128);
///
/// impl TraceIdLike for MyTraceId {
///     fn to_u128(&self) -> u128 {
///         self.0
///     }
/// }
/// ```
pub trait TraceIdLike: PartialEq + Eq {
    /// Converts the trace ID to a 128-bit unsigned integer.
    ///
    /// The conversion should be deterministic: the same trace ID must always produce
    /// the same `u128` value. Typically implemented by interpreting the trace ID's
    /// bytes as a big-endian integer.
    fn to_u128(&self) -> u128;
}

/// A trait for accessing span attribute key-value pairs.
///
/// Provides methods for retrieving the key and value of a span attribute.
pub trait AttributeLike {
    /// The type of the value that implements `ValueLike`.
    type Value: ValueLike;

    /// Returns the attribute key as a string.
    fn key(&self) -> &str;

    /// Returns a reference to the attribute value.
    fn value(&self) -> &Self::Value;
}

/// A trait for extracting typed values from attribute values.
///
/// Provides methods for converting attribute values to common types used in sampling logic.
pub trait ValueLike {
    /// Extracts a float value if the value can be represented as `f64`.
    ///
    /// Returns `Some(f64)` for numeric types, `None` otherwise.
    fn extract_float(&self) -> Option<f64>;

    /// Extracts a string representation of the value.
    ///
    /// Returns `Some(Cow<str>)` for types that can be converted to strings, `None` otherwise.
    fn extract_string(&self) -> Option<Cow<'_, str>>;
}

/// A trait for creating sampling attributes.
///
/// This trait abstracts the creation of attributes for sampling tags,
/// allowing different implementations for different attribute types.
pub trait AttributeFactory {
    /// The type of attribute created by this factory.
    type Attribute: Sized;

    /// Creates an attribute with an i64 value.
    fn create_i64(&self, key: &'static str, value: i64) -> Self::Attribute;

    /// Creates an attribute with an f64 value.
    fn create_f64(&self, key: &'static str, value: f64) -> Self::Attribute;

    /// Creates an attribute with a string value.
    fn create_string(&self, key: &'static str, value: Cow<'static, str>) -> Self::Attribute;
}

/// A trait for accessing span properties needed for sampling decisions.
///
/// Provides methods for retrieving span metadata like operation name, service, environment,
/// resource name, and status codes used by sampling rules.
pub trait SpanProperties {
    /// The type of attribute that implements `AttributeLike`.
    type Attribute: AttributeLike;

    /// Returns the operation name for the span.
    ///
    /// The operation name is derived from span attributes and kind according to
    /// OpenTelemetry semantic conventions.
    fn operation_name(&self) -> Cow<'_, str>;

    /// Returns the service name for the span.
    ///
    /// The service name is extracted from resource attributes.
    fn service(&self) -> Cow<'_, str>;

    /// Returns the environment name for the span.
    ///
    /// The environment is extracted from span or resource attributes.
    fn env(&self) -> Cow<'_, str>;

    /// Returns the resource name for the span.
    ///
    /// The resource name is derived from span attributes and kind.
    fn resource(&self) -> Cow<'_, str>;

    /// Returns the HTTP status code if present.
    ///
    /// Returns `None` if the span does not have an HTTP status code attribute.
    fn status_code(&self) -> Option<u32>;

    /// Returns an iterator over span attributes.
    fn attributes<'a>(&'a self) -> impl Iterator<Item = &'a Self::Attribute>
    where
        Self: 'a;

    /// Returns an alternate key for the given attribute key.
    ///
    /// This is used for mapping between different attribute naming conventions
    /// (e.g., OpenTelemetry to Datadog). Returns `Some(alternate_key)` if a mapping exists,
    /// or `None` if the attribute key has no alternate mapping.
    fn get_alternate_key<'b>(&self, key: &'b str) -> Option<Cow<'b, str>>;
}

/// A trait for accessing sampling data, combining trace ID and span properties.
///
/// This trait provides unified access to both the trace ID and span properties
/// needed for making sampling decisions.
pub trait SamplingData {
    /// The type that implements `TraceIdLike`.
    type TraceId: TraceIdLike;

    /// The type that implements `SpanProperties`.
    type Properties<'a>: SpanProperties
    where
        Self: 'a;

    /// Returns whether the parent span was sampled.
    ///
    /// Returns:
    /// - `Some(true)` if the parent span was sampled
    /// - `Some(false)` if the parent span was not sampled
    /// - `None` if there is no parent sampling information
    fn is_parent_sampled(&self) -> Option<bool>;

    /// Returns a reference to the trace ID.
    fn trace_id(&self) -> &Self::TraceId;

    /// Returns the span properties via a callback.
    ///
    /// This method constructs the span properties and passes them to the provided
    /// callback function. The properties are only valid for the duration of the callback.
    fn with_span_properties<S, T, F>(&self, s: &S, f: F) -> T
    where
        F: Fn(&S, &Self::Properties<'_>) -> T;
}
