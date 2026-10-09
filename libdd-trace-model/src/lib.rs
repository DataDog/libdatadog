// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared data model for Datadog trace data: the [`TraceText`] / [`TraceBytes`] storage types,
//! typed [`AttributeValue`]s, and the [`ValueMap`] / [`ValueArray`] traits over the containers
//! backing them. Storage crates implement these traits for their own types.

extern crate alloc;

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
