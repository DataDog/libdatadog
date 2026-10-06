// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern crate alloc;

use alloc::sync::Arc;
use core::borrow::Borrow;
use core::fmt::Debug;
use core::hash::Hash;

pub trait TraceText: Borrow<str> + Clone + Eq + Hash + Debug + From<String> {
    fn from_static(s: &'static str) -> Self;

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

impl<'a> TraceText for alloc::borrow::Cow<'a, str> {
    fn from_static(s: &'static str) -> Self {
        alloc::borrow::Cow::Borrowed(s)
    }
}

#[cfg(feature = "tinybytes")]
impl TraceText for libdd_tinybytes::BytesString {
    fn from_static(s: &'static str) -> Self {
        Self::from_static(s)
    }

    fn as_str(&self) -> &str {
        self.borrow()
    }
}

pub trait TraceBytes: Borrow<[u8]> + Clone + Debug + PartialEq {}

impl<T: Borrow<[u8]> + Clone + Debug + PartialEq> TraceBytes for T {}

pub type Value<A> = AttributeValue<<A as Attributes>::Values>;

pub trait Attributes {
    type Text: TraceText;
    type Bytes: TraceBytes;
    type Values: ValueTypes<Text = Self::Text, Bytes = Self::Bytes>;

    fn attribute(&self, key: &str) -> Option<&Value<Self>>;
    // In place mutation and removal, return False to remove.
    fn retain_attributes(&mut self, f: impl FnMut(&Self::Text, &mut Value<Self>) -> bool);

    fn attribute_mut(&mut self, key: &str) -> Option<&mut Value<Self>>;
    fn set_attribute(&mut self, key: impl Into<Self::Text>, value: Value<Self>);
}

pub trait Span: Attributes {
    fn service(&self) -> &str;
    fn resource(&self) -> &str;
    fn r#type(&self) -> &str;

    fn set_service(&mut self, value: impl Into<Self::Text>);
    fn set_resource(&mut self, value: impl Into<Self::Text>);
}

// TODO: What other things do we need to be able to do to attribute value maps and lists?
pub trait ValueMap<T: ValueTypes> {
    fn try_get(&self, key: &str) -> Option<&AttributeValue<T>>;
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a str, &'a AttributeValue<T>)>
    where
        T: 'a;
}

pub trait ValueArray<T: ValueTypes> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = &'a AttributeValue<T>>
    where
        T: 'a;
}

impl<T: ValueTypes> ValueArray<T> for Vec<AttributeValue<T>> {
    fn iter<'a>(&'a self) -> impl Iterator<Item = &'a AttributeValue<T>>
    where
        T: 'a,
    {
        self.as_slice().iter()
    }
}

pub trait ValueTypes: Sized {
    type Text: TraceText;
    type Bytes: TraceBytes;
    type Array: ValueArray<Self> + Clone + PartialEq + Debug;
    type Map: ValueMap<Self> + Clone + PartialEq + Debug;
}

pub enum AttributeValue<T: ValueTypes> {
    String(T::Text),
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(T::Bytes),
    Array(T::Array),
    KeyValueList(T::Map),
}

impl<T: ValueTypes> Clone for AttributeValue<T> {
    fn clone(&self) -> Self {
        match self {
            Self::String(arg0) => Self::String(arg0.clone()),
            Self::Bool(arg0) => Self::Bool(arg0.clone()),
            Self::Int(arg0) => Self::Int(arg0.clone()),
            Self::Float(arg0) => Self::Float(arg0.clone()),
            Self::Bytes(arg0) => Self::Bytes(arg0.clone()),
            Self::Array(arg0) => Self::Array(arg0.clone()),
            Self::KeyValueList(arg0) => Self::KeyValueList(arg0.clone()),
        }
    }
}

// TODO: Some implementations of PartialEq (like VecMap) do allocs / could be slow
impl<T: ValueTypes> PartialEq for AttributeValue<T> {
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
