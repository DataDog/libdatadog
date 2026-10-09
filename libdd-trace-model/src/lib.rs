// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared data model for Datadog trace data: the [`TraceText`] / [`TraceBytes`] storage types,
//! typed [`AttributeValue`]s, the [`ValueMap`] / [`ValueArray`] traits over the containers
//! backing them, and read access to spans ([`Span`]) and trace chunks ([`TraceChunk`]). Storage
//! crates implement these traits for their own types.

extern crate alloc;

mod chunk_span_view;
pub use chunk_span_view::ChunkSpanView;

use alloc::sync::Arc;
use core::borrow::Borrow;
use core::fmt::Debug;
use core::hash::{BuildHasher, Hash};
use std::collections::HashMap;

pub trait TraceText: Borrow<str> + Clone + Eq + Hash + Debug + From<String> {
    /// Create a `TraceText` from a static string
    fn from_static(s: &'static str) -> Self;

    /// Get a `&str` representation of this `TraceText`
    fn as_str(&self) -> &str {
        self.borrow()
    }
}

impl TraceText for String {
    fn from_static(s: &'static str) -> Self {
        s.to_owned()
    }
}

impl TraceText for Arc<str> {
    fn from_static(s: &'static str) -> Self {
        Self::from(s)
    }
}

impl TraceText for alloc::borrow::Cow<'_, str> {
    fn from_static(s: &'static str) -> Self {
        alloc::borrow::Cow::Borrowed(s)
    }
}

pub trait TraceBytes: Borrow<[u8]> + Clone + Debug + PartialEq {}

impl<T: Borrow<[u8]> + Clone + Debug + PartialEq> TraceBytes for T {}

pub type Value<A> = AttributeValue<<A as Attributes>::Values>;

/// Metric set to 1 by the tracer on top-level spans.
const TRACER_TOP_LEVEL_KEY: &str = "_dd.top_level";
/// Metric set to 1 by the agent / trace-utils on top-level spans.
const TOP_LEVEL_KEY: &str = "_top_level";
/// Metric set to 1 on spans that should get trace metrics computed.
const MEASURED_KEY: &str = "_dd.measured";
/// Metric present (and non-negative) on partial snapshots of long-running spans.
const PARTIAL_VERSION_KEY: &str = "_dd.partial_version";
/// Tag name of the span kind, promoted to [`Span::span_kind`].
pub const SPAN_KIND_KEY: &str = "span.kind";
/// Tag name of the environment, promoted to [`Span::env`].
pub const ENV_KEY: &str = "env";
/// Tag name of the application version, promoted to [`Span::version`].
pub const VERSION_KEY: &str = "version";
/// Tag name of the instrumentation component, promoted to [`Span::component`].
pub const COMPONENT_KEY: &str = "component";
/// The span tags promoted to dedicated fields, see [`Attributes`].
pub const PROMOTED_SPAN_TAGS: [&str; 4] = [SPAN_KIND_KEY, ENV_KEY, VERSION_KEY, COMPONENT_KEY];

/// Tag name of the origin, promoted to [`TraceChunk::origin`].
pub const ORIGIN_KEY: &str = "_dd.origin";
/// Tag name of the sampling priority, promoted to [`TraceChunk::priority`].
pub const SAMPLING_PRIORITY_KEY: &str = "_sampling_priority_v1";
/// Tag name of the sampling decision maker, promoted to [`TraceChunk::sampling_mechanism`]. Its
/// value is the mechanism, negated (e.g. `"-4"`).
pub const DECISION_MAKER_KEY: &str = "_dd.p.dm";
/// The chunk tags promoted to dedicated fields, see [`Attributes`].
pub const PROMOTED_CHUNK_TAGS: [&str; 3] = [ORIGIN_KEY, SAMPLING_PRIORITY_KEY, DECISION_MAKER_KEY];

/// Read access to something that has key value "attributes" on it like a span, span event, or
/// trace chunk.
///
/// # Promoted tags
///
/// Tags with a dedicated field (see [`PROMOTED_SPAN_TAGS`] and [`PROMOTED_CHUNK_TAGS`]) live only
/// in that field, never in the attributes: the attribute getters don't see them. Use
/// [`Span::tag_str`] / [`TraceChunk::tag_str`] to look a tag up by name wherever it is stored.
pub trait Attributes {
    type Text: TraceText;
    type Bytes: TraceBytes;
    type Values: ValueTypes<Text = Self::Text, Bytes = Self::Bytes>;

    /// Get the string value of the attribute for a given key, `None` if it is missing or not a
    /// string.
    fn attribute_str(&self, key: &str) -> Option<&str>;

    /// Get the numeric value of the attribute for a given key, `None` if it is missing or not a
    /// number. Integers are converted to `f64`.
    fn attribute_f64(&self, key: &str) -> Option<f64>;

    /// Get the integer value of the attribute for a given key, `None` if it is missing, not a
    /// number, or a float that isn't exactly an integer.
    ///
    /// The default goes through [`Attributes::attribute_f64`], which suits storage that only holds
    /// floats. Storage holding integers should override it so values above 2^53 stay exact.
    fn attribute_i64(&self, key: &str) -> Option<i64> {
        self.attribute_f64(key).and_then(f64_to_exact_i64)
    }
}

/// Converts `f` to an `i64` if it is exactly an integer within range.
#[must_use]
pub fn f64_to_exact_i64(f: f64) -> Option<i64> {
    // -2^63 and 2^63 are both exactly representable, and `f` is checked to be an integer within
    // [-2^63, 2^63), so the casts below are exact.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    {
        (f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64).then_some(f as i64)
    }
}

/// Iteration over attributes for storage that holds [`AttributeValue`]s directly.
///
/// Implementations must yield each key at most once, with the value lookups return for it, even if
/// the underlying storage tolerates duplicate entries.
pub trait AttributesIter: Attributes {
    /// Iterate over all `(key, value)` attribute pairs, each key at most once
    fn attributes(&self) -> impl Iterator<Item = (&str, &Value<Self>)>;

    /// Returns true if an attribute (of any type) exists for `key`
    fn has_attribute(&self, key: &str) -> bool {
        self.attributes().any(|(k, _)| k == key)
    }
}

/// A timestamped event attached to a span, carrying its own attributes.
pub trait SpanEvent: Attributes {
    fn name(&self) -> &str;
    fn time_unix_nano(&self) -> u64;
}

/// A link from a span to another span, possibly in another trace, carrying its own attributes.
pub trait SpanLink: Attributes {
    /// The 128-bit trace id of the linked span
    fn trace_id(&self) -> u128;
    /// The span id of the linked span
    fn span_id(&self) -> u64;
    /// The W3C tracestate of the linked span, empty if unset
    fn tracestate(&self) -> &str;
    /// The W3C trace flags of the linked span
    fn flags(&self) -> u32;
}

/// Read access to a span.
pub trait Span: Attributes {
    type SpanEvent: SpanEvent<Values = Self::Values>;
    type SpanLink: SpanLink<Values = Self::Values>;

    fn service(&self) -> &str;
    fn resource(&self) -> &str;
    /// The operation name
    fn name(&self) -> &str;
    /// The span type (e.g. "web", "db")
    fn typ(&self) -> &str;
    /// The id of this span
    fn span_id(&self) -> u64;
    /// The span id of this span's parent, 0 for a trace root
    fn parent_id(&self) -> u64;
    /// The start timestamp, in nanoseconds since the unix epoch
    fn start(&self) -> i64;
    /// The duration, in nanoseconds
    fn duration(&self) -> i64;
    /// Returns true if the span is an error
    fn is_error(&self) -> bool;

    /// The span kind (e.g. "server", "client"), `None` if unset.
    fn span_kind(&self) -> Option<&str>;
    /// The `env` the span was emitted from, `None` if unset.
    fn env(&self) -> Option<&str>;
    /// The application `version`, `None` if unset.
    fn version(&self) -> Option<&str>;
    /// The instrumentation `component`, `None` if unset.
    fn component(&self) -> Option<&str>;

    /// Look up a string tag by name: promoted tags (see [`Attributes`]) are read from their
    /// dedicated field, any other key from the string attributes.
    fn tag_str(&self, key: &str) -> Option<&str> {
        match key {
            SPAN_KIND_KEY => self.span_kind(),
            ENV_KEY => self.env(),
            VERSION_KEY => self.version(),
            COMPONENT_KEY => self.component(),
            _ => self.attribute_str(key),
        }
    }

    /// Iterate over the events attached to this span
    fn span_events(&self) -> impl Iterator<Item = &Self::SpanEvent>;
    /// Iterate over the links attached to this span
    fn span_links(&self) -> impl Iterator<Item = &Self::SpanLink>;

    /// Returns true if the span is a trace root
    fn is_trace_root(&self) -> bool {
        self.parent_id() == 0
    }

    /// Returns true if the span is marked as top-level, by either the tracer or trace-utils
    fn has_top_level(&self) -> bool {
        self.attribute_f64(TRACER_TOP_LEVEL_KEY) == Some(1.0)
            || self.attribute_f64(TOP_LEVEL_KEY) == Some(1.0)
    }

    /// Returns true if the span should be measured (i.e., it should get trace metrics computed)
    fn is_measured(&self) -> bool {
        self.attribute_f64(MEASURED_KEY) == Some(1.0)
    }

    /// Returns true if the span is a partial snapshot of a long-running span
    fn is_partial_snapshot(&self) -> bool {
        self.attribute_f64(PARTIAL_VERSION_KEY)
            .is_some_and(|v| v >= 0.0)
    }
}

/// A tag value borrowed from a span: a promoted tag read from its dedicated field, or an
/// attribute.
pub enum TagValue<'a, T: ValueTypes> {
    /// A promoted tag stored in a dedicated field (e.g. `env`).
    Str(&'a str),
    /// A value stored in the attributes.
    Value(&'a AttributeValue<T>),
}

impl<'a, T: ValueTypes> TagValue<'a, T> {
    /// The string value, `None` if this isn't a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&'a str> {
        match *self {
            Self::Str(s) => Some(s),
            Self::Value(v) => v.as_str(),
        }
    }
}

impl<T: ValueTypes> Clone for TagValue<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ValueTypes> Copy for TagValue<'_, T> {}

impl<T: ValueTypes> Debug for TagValue<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Str(s) => f.debug_tuple("Str").field(s).finish(),
            Self::Value(v) => f.debug_tuple("Value").field(v).finish(),
        }
    }
}

/// Iteration over every tag of a span by name, the way v0.4 `meta`/`metrics` would list them.
///
/// This should only really be used for customer-facing config to maintain backwards compatibility.
/// For example: Customer defined sampling rules where they may reference promoted fields at
/// runtime. Static implementations should prefer to explicitly act on promoted fields or generic
/// attributes.
pub trait SpanTags: Span + AttributesIter {
    /// Iterate over all `(name, value)` tags, each name at most once: the attributes, plus the
    /// promoted tags (see [`Attributes`]) read from their dedicated fields.
    fn tags(&self) -> impl Iterator<Item = (&str, TagValue<'_, Self::Values>)> {
        let promoted = [
            (SPAN_KIND_KEY, self.span_kind()),
            (ENV_KEY, self.env()),
            (VERSION_KEY, self.version()),
            (COMPONENT_KEY, self.component()),
        ]
        .into_iter()
        .filter_map(|(key, value)| Some((key, TagValue::Str(value?))));
        // Promoted tags never live in the attributes, so the two never overlap.
        self.attributes()
            .map(|(key, value)| (key, TagValue::Value(value)))
            .chain(promoted)
    }

    /// Returns true if a tag (of any type) exists for `name`
    fn has_tag(&self, name: &str) -> bool {
        match name {
            SPAN_KIND_KEY | ENV_KEY | VERSION_KEY | COMPONENT_KEY => self.tag_str(name).is_some(),
            _ => self.has_attribute(name),
        }
    }
}

/// Read access to a trace chunk: a group of spans sharing the same trace id, plus chunk-level
/// attributes common to all of them.
pub trait TraceChunk: Attributes {
    type Span: Span<Values = Self::Values>;

    /// The 128-bit trace id shared by every span in the chunk
    fn trace_id(&self) -> u128;
    /// The `_dd.origin` value shared by every span in the chunk, empty if unset
    fn origin(&self) -> &str;
    /// Iterate over the spans of this chunk
    fn spans(&self) -> impl Iterator<Item = &Self::Span>;
    /// The sampling priority of the chunk, `None` if no decision was made
    fn priority(&self) -> Option<i32>;
    /// The sampling mechanism (decision maker) of the chunk, `None` if unset
    fn sampling_mechanism(&self) -> Option<u32>;
    /// Returns true if the tracer dropped this trace, keeping only some of its spans (e.g. for
    /// single span sampling). Such a chunk must be treated as rejected whatever its priority.
    fn dropped_trace(&self) -> bool;

    /// Look up a string tag by name: `_dd.origin` is read from [`TraceChunk::origin`], any other
    /// key from the string attributes. The numeric promoted tags have no string form here; read
    /// [`TraceChunk::priority`] and [`TraceChunk::sampling_mechanism`] instead.
    fn tag_str(&self, key: &str) -> Option<&str> {
        match key {
            ORIGIN_KEY => Some(self.origin()).filter(|origin| !origin.is_empty()),
            SAMPLING_PRIORITY_KEY | DECISION_MAKER_KEY => None,
            _ => self.attribute_str(key),
        }
    }
}

/// Read access to a map of attribute values (the payload of [`AttributeValue::KeyValueList`]).
///
/// Keys are unique: implementations backed by storage that tolerates duplicate entries must hide
/// the shadowed ones, so that [`ValueMap::iter`] yields each key once with the value
/// [`ValueMap::get`] returns, and [`ValueMap::len`] counts distinct keys. The same holds for
/// [`ValueMapMut::iter_mut`] and [`ValueMapMut::retain`].
pub trait ValueMap<T: ValueTypes> {
    /// Get the value for a given key, `None` if it is missing
    fn get(&self, key: &str) -> Option<&AttributeValue<T>>;
    /// Returns true if a value exists for `key`
    fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    /// The number of distinct keys
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Iterate over all `(key, value)` pairs
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a str, &'a AttributeValue<T>)>
    where
        T: 'a;
}

/// Write access to a map of attribute values.
pub trait ValueMapMut<T: ValueTypes>: ValueMap<T> {
    /// Get a mutable reference to the value for a given key
    fn get_mut(&mut self, key: &str) -> Option<&mut AttributeValue<T>>;
    /// Set the value for `key`, returning the value it replaced, if any
    fn insert(&mut self, key: T::Text, value: AttributeValue<T>) -> Option<AttributeValue<T>>;
    /// Remove the value for `key`, returning it if it was present
    fn remove(&mut self, key: &str) -> Option<AttributeValue<T>>;
    /// Keep only the entries for which `f` returns `true`; `f` may mutate the values it keeps.
    fn retain(&mut self, f: impl FnMut(&str, &mut AttributeValue<T>) -> bool);
    /// Iterate mutably over all `(key, value)` pairs
    fn iter_mut<'a>(&'a mut self) -> impl Iterator<Item = (&'a str, &'a mut AttributeValue<T>)>
    where
        T: 'a;
}

/// Read access to a list of attribute values (the payload of [`AttributeValue::Array`]).
pub trait ValueArray<T: ValueTypes> {
    /// Get the value at `index`, `None` if out of bounds
    fn get(&self, index: usize) -> Option<&AttributeValue<T>>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn iter<'a>(&'a self) -> impl Iterator<Item = &'a AttributeValue<T>>
    where
        T: 'a;
}

/// Write access to a list of attribute values.
pub trait ValueArrayMut<T: ValueTypes>: ValueArray<T> {
    /// Get a mutable reference to the value at `index`, `None` if out of bounds
    fn get_mut(&mut self, index: usize) -> Option<&mut AttributeValue<T>>;
    /// Append a value to the end of the list
    fn push(&mut self, value: AttributeValue<T>);
    /// Keep only the values for which `f` returns `true`; `f` may mutate the values it keeps.
    fn retain(&mut self, f: impl FnMut(&mut AttributeValue<T>) -> bool);
    fn iter_mut<'a>(&'a mut self) -> impl Iterator<Item = &'a mut AttributeValue<T>>
    where
        T: 'a;
}

impl<T: ValueTypes, S: BuildHasher> ValueMap<T> for HashMap<T::Text, AttributeValue<T>, S> {
    fn get(&self, key: &str) -> Option<&AttributeValue<T>> {
        Self::get(self, key)
    }

    fn contains_key(&self, key: &str) -> bool {
        Self::contains_key(self, key)
    }

    fn len(&self) -> usize {
        Self::len(self)
    }

    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a str, &'a AttributeValue<T>)>
    where
        T: 'a,
    {
        Self::iter(self).map(|(k, v)| (k.as_str(), v))
    }
}

impl<T: ValueTypes, S: BuildHasher> ValueMapMut<T> for HashMap<T::Text, AttributeValue<T>, S> {
    fn get_mut(&mut self, key: &str) -> Option<&mut AttributeValue<T>> {
        Self::get_mut(self, key)
    }

    fn insert(&mut self, key: T::Text, value: AttributeValue<T>) -> Option<AttributeValue<T>> {
        Self::insert(self, key, value)
    }

    fn remove(&mut self, key: &str) -> Option<AttributeValue<T>> {
        Self::remove(self, key)
    }

    fn retain(&mut self, mut f: impl FnMut(&str, &mut AttributeValue<T>) -> bool) {
        Self::retain(self, |k, v| f(k.as_str(), v));
    }

    fn iter_mut<'a>(&'a mut self) -> impl Iterator<Item = (&'a str, &'a mut AttributeValue<T>)>
    where
        T: 'a,
    {
        Self::iter_mut(self).map(|(k, v)| (k.as_str(), v))
    }
}

impl<T: ValueTypes> ValueArray<T> for Vec<AttributeValue<T>> {
    fn get(&self, index: usize) -> Option<&AttributeValue<T>> {
        self.as_slice().get(index)
    }

    fn len(&self) -> usize {
        Self::len(self)
    }

    fn iter<'a>(&'a self) -> impl Iterator<Item = &'a AttributeValue<T>>
    where
        T: 'a,
    {
        self.as_slice().iter()
    }
}

impl<T: ValueTypes> ValueArrayMut<T> for Vec<AttributeValue<T>> {
    fn get_mut(&mut self, index: usize) -> Option<&mut AttributeValue<T>> {
        self.as_mut_slice().get_mut(index)
    }

    fn push(&mut self, value: AttributeValue<T>) {
        Self::push(self, value);
    }

    fn retain(&mut self, f: impl FnMut(&mut AttributeValue<T>) -> bool) {
        self.retain_mut(f);
    }

    fn iter_mut<'a>(&'a mut self) -> impl Iterator<Item = &'a mut AttributeValue<T>>
    where
        T: 'a,
    {
        self.as_mut_slice().iter_mut()
    }
}

pub trait ValueTypes: Sized {
    type Text: TraceText;
    type Bytes: TraceBytes;
    type Array: ValueArrayMut<Self> + Clone + Debug;
    type Map: ValueMapMut<Self> + Clone + Debug;
}

/// [`ValueTypes`] whose containers implement [`PartialEq`], making [`AttributeValue`] comparable
/// with `==`.
///
/// The bounds live on a subtrait rather than on the `PartialEq` impl: a `where T::Map: PartialEq`
/// clause is recursive (the map holds `AttributeValue<T>`s) and overflows the trait solver.
pub trait ValueTypesEq: ValueTypes<Array: PartialEq, Map: PartialEq> {}

/// The value in a map of attributes
pub enum AttributeValue<T: ValueTypes> {
    String(T::Text),
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(T::Bytes),
    Array(T::Array),
    KeyValueList(T::Map),
}

impl<T: ValueTypes> AttributeValue<T> {
    /// The string value, `None` if this isn't a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// The integer value, `None` if this isn't a number or is a float that isn't exactly an
    /// integer.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Float(f) => f64_to_exact_i64(*f),
            _ => None,
        }
    }

    /// The numeric value, `None` if this isn't a number. Integers are converted to `f64`.
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            // Lossy above 2^53, which is acceptable for the metric-like values read as floats.
            #[allow(clippy::cast_precision_loss)]
            Self::Int(i) => Some(*i as f64),
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }
}

impl<T: ValueTypes> AttributeValue<T> {
    /// Structural equality that doesn't require `PartialEq` on the backing containers.
    ///
    /// Maps are compared with [`value_maps_slow_eq`]. This is quadratic in the size of nested
    /// maps, so prefer `==` where it is available.
    pub fn slow_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::String(l), Self::String(r)) => l == r,
            (Self::Bool(l), Self::Bool(r)) => l == r,
            (Self::Int(l), Self::Int(r)) => l == r,
            (Self::Float(l), Self::Float(r)) => l == r,
            (Self::Bytes(l), Self::Bytes(r)) => l == r,
            (Self::Array(l), Self::Array(r)) => {
                l.len() == r.len() && l.iter().zip(r.iter()).all(|(l, r)| l.slow_eq(r))
            }
            (Self::KeyValueList(l), Self::KeyValueList(r)) => value_maps_slow_eq(l, r),
            _ => false,
        }
    }
}

/// Compares two value maps for equality, ignoring order. Doesn't allocate, but is quadratic in the
/// size of the maps.
pub fn value_maps_slow_eq<T: ValueTypes, M: ValueMap<T>>(lhs: &M, rhs: &M) -> bool {
    lhs.len() == rhs.len()
        && lhs
            .iter()
            .all(|(k, l)| rhs.get(k).is_some_and(|r| l.slow_eq(r)))
}

impl<T: ValueTypes> Clone for AttributeValue<T> {
    fn clone(&self) -> Self {
        match self {
            Self::String(arg0) => Self::String(arg0.clone()),
            Self::Bool(arg0) => Self::Bool(*arg0),
            Self::Int(arg0) => Self::Int(*arg0),
            Self::Float(arg0) => Self::Float(*arg0),
            Self::Bytes(arg0) => Self::Bytes(arg0.clone()),
            Self::Array(arg0) => Self::Array(arg0.clone()),
            Self::KeyValueList(arg0) => Self::KeyValueList(arg0.clone()),
        }
    }
}

// Only available when the backing containers are comparable: some (like `VecMap`) only offer
// `PartialEq` in tests since comparing them allocates. Use `AttributeValue::slow_eq` otherwise.
impl<T: ValueTypesEq> PartialEq for AttributeValue<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::String(l0), Self::String(r0)) => l0 == r0,
            (Self::Bool(l0), Self::Bool(r0)) => l0 == r0,
            (Self::Int(l0), Self::Int(r0)) => l0 == r0,
            (Self::Float(l0), Self::Float(r0)) => l0 == r0,
            (Self::Bytes(l0), Self::Bytes(r0)) => l0 == r0,
            (Self::Array(l0), Self::Array(r0)) => l0 == r0,
            (Self::KeyValueList(l0), Self::KeyValueList(r0)) => l0 == r0,
            _ => false,
        }
    }
}

impl<T: ValueTypes> Debug for AttributeValue<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::String(arg0) => f.debug_tuple("String").field(arg0).finish(),
            Self::Bool(arg0) => f.debug_tuple("Bool").field(arg0).finish(),
            Self::Int(arg0) => f.debug_tuple("Int").field(arg0).finish(),
            Self::Float(arg0) => f.debug_tuple("Float").field(arg0).finish(),
            Self::Bytes(arg0) => f.debug_tuple("Bytes").field(arg0).finish(),
            Self::Array(arg0) => f.debug_tuple("Array").field(arg0).finish(),
            Self::KeyValueList(arg0) => f.debug_tuple("KeyValueList").field(arg0).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct HashData;
    impl ValueTypes for HashData {
        type Text = String;
        type Bytes = Vec<u8>;
        type Array = Vec<AttributeValue<Self>>;
        type Map = HashMap<String, AttributeValue<Self>>;
    }
    impl ValueTypesEq for HashData {}

    type V = AttributeValue<HashData>;
    type Map = HashMap<String, V>;

    fn str_value(s: &str) -> V {
        V::String(s.to_owned())
    }

    #[test]
    fn exact_i64_conversion() {
        assert_eq!(f64_to_exact_i64(-3.0), Some(-3));
        assert_eq!(f64_to_exact_i64(3.5), None);
        assert_eq!(f64_to_exact_i64(f64::NAN), None);
        assert_eq!(f64_to_exact_i64(f64::INFINITY), None);
        // ±2^63 as literals: `powi` is not exact under Miri.
        assert_eq!(
            f64_to_exact_i64(-9_223_372_036_854_775_808.0),
            Some(i64::MIN)
        );
        assert_eq!(f64_to_exact_i64(9_223_372_036_854_775_808.0), None);
        assert_eq!(V::Int(i64::MAX).as_i64(), Some(i64::MAX));
        assert_eq!(V::Float(2.0).as_i64(), Some(2));
        assert_eq!(str_value("2").as_i64(), None);
    }

    #[test]
    fn hash_map_value_map_mut() {
        let mut map = Map::new();
        assert!(ValueMap::is_empty(&map));
        assert_eq!(ValueMapMut::insert(&mut map, "a".into(), V::Int(1)), None);
        assert_eq!(
            ValueMapMut::insert(&mut map, "a".into(), V::Int(2)),
            Some(V::Int(1))
        );
        ValueMapMut::insert(&mut map, "b".into(), str_value("x"));
        assert_eq!(ValueMap::len(&map), 2);
        assert_eq!(ValueMap::get(&map, "a"), Some(&V::Int(2)));

        ValueMapMut::retain(&mut map, |k, v| {
            if let V::Int(i) = v {
                *i += 1;
            }
            k != "b"
        });
        assert!(!ValueMap::contains_key(&map, "b"));
        assert_eq!(ValueMapMut::remove(&mut map, "a"), Some(V::Int(3)));
        assert!(ValueMap::is_empty(&map));
    }

    #[test]
    fn slow_eq_matches_eq() {
        let nested = |v: i64| {
            let mut map = Map::new();
            map.insert("n".into(), V::Array(vec![V::Int(v), str_value("s")]));
            map.insert("m".into(), V::Bool(true));
            V::KeyValueList(map)
        };
        let values = [
            nested(1),
            nested(2),
            V::Int(1),
            V::Float(1.0),
            str_value("s"),
        ];
        for l in &values {
            for r in &values {
                assert_eq!(l.slow_eq(r), l == r, "{l:?} vs {r:?}");
            }
        }

        let mut extra = Map::new();
        extra.insert("m".into(), V::Bool(true));
        let mut smaller = extra.clone();
        extra.insert("other".into(), V::Bool(true));
        assert!(!value_maps_slow_eq(&extra, &smaller));
        smaller.insert("other".into(), V::Bool(true));
        assert!(value_maps_slow_eq(&extra, &smaller));
    }
}
