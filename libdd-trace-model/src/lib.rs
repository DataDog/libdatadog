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

pub trait TraceBytes: Borrow<[u8]> + Clone + Debug + From<Vec<u8>> {}

impl<T: Borrow<[u8]> + Clone + Debug + From<Vec<u8>>> TraceBytes for T {}

pub type Value<A> = AttributeValue<<A as Attributes>::Text, <A as Attributes>::Bytes>;

pub trait Attributes {
    type Text: TraceText;
    type Bytes: TraceBytes;

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

#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue<S, B> {
    String(S),
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(B),
    Array(Vec<Self>),
    KeyValueList(Vec<(S, Self)>),
}
//TODO: is it okay to basically require users use this exact attribute value implementation?
// OR use something more like Opus suggests: "Return a borrowed view by value. For example enum AttrRef<'a, T, B> { Str(&'a T), Float(f64), Int(i64), Bool(bool), Bytes(&'a B), Opaque }, with nested lists and maps handled as Opaque or through an associated type. Any type can build this on the fly."
// With the caveat the above would need some modifications to make it possible for logic to do things like recurse _into_ the keyvalue and array types (not shown here yet)
