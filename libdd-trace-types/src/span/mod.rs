// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod v04;
pub mod v05;
pub mod v1;
pub mod vec_map;

use libdd_tinybytes::{Bytes, BytesString};
use serde::Serialize;
use std::borrow::{Borrow, Cow};
use std::fmt::{self, Debug};
use std::hash::Hash;
use std::marker::PhantomData;

/// Trait representing the requirements for a type to be used as a Span "string" type.
/// Note: Borrow<str> is not required by the derived traits, but allows to access HashMap elements
/// from a static str and check if the string is empty.
pub trait SpanText: Debug + Eq + Hash + Borrow<str> + Serialize + Default {
    fn from_static_str(value: &'static str) -> Self;

    /// Copies this text into an owned [`BytesString`].
    ///
    /// Used by the v0.5 conversion, whose shared dictionary always owns its strings so it
    /// can hold both interned span text and dynamically-built JSON (span links / events).
    /// The default copies the bytes; owned text types (e.g. `BytesString`) should override
    /// with a cheaper reference-counted clone.
    fn to_bytes_string(&self) -> BytesString {
        BytesString::from(<Self as Borrow<str>>::borrow(self).to_string())
    }

    fn from_owned(value: String) -> Self;
}

impl SpanText for Cow<'_, str> {
    fn from_static_str(value: &'static str) -> Self {
        Cow::Borrowed(value)
    }

    fn from_owned(value: String) -> Self {
        Cow::Owned(value)
    }
}

impl SpanText for BytesString {
    fn from_static_str(value: &'static str) -> Self {
        BytesString::from_static(value)
    }

    fn to_bytes_string(&self) -> BytesString {
        self.clone()
    }

    fn from_owned(value: String) -> Self {
        BytesString::from_string(value)
    }
}

pub trait SpanBytes: Debug + Eq + Hash + Borrow<[u8]> + Serialize + Default + Clone {
    fn from_static_bytes(value: &'static [u8]) -> Self;
}

impl SpanBytes for &[u8] {
    fn from_static_bytes(value: &'static [u8]) -> Self {
        value
    }
}

impl SpanBytes for Bytes {
    fn from_static_bytes(value: &'static [u8]) -> Self {
        Bytes::from_static(value)
    }
}

/// Trait representing a tuple of (Text, Bytes) types used for different underlying data structures.
/// Defines an associated Text type for string data and a Bytes type for binary data.
/// Also note that the Clone and PartialEq bounds are only present for tests.
pub trait TraceData: Default + Clone + Debug + PartialEq {
    type Text: SpanText;
    type Bytes: SpanBytes;
}

/// TraceData implementation using `Bytes` and `BytesString`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct BytesData;
impl TraceData for BytesData {
    type Text = BytesString;
    type Bytes = Bytes;
}

/// TraceData implementation using `&str` and `&[u8]`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct SliceData<'a>(PhantomData<&'a u8>);
impl<'a> TraceData for SliceData<'a> {
    type Text = Cow<'a, str>;
    type Bytes = &'a [u8];
}

#[derive(Debug)]
pub struct SpanKeyParseError {
    pub message: String,
}

impl SpanKeyParseError {
    pub fn new(message: impl Into<String>) -> Self {
        SpanKeyParseError {
            message: message.into(),
        }
    }
}
impl fmt::Display for SpanKeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpanKeyParseError: {}", self.message)
    }
}
impl std::error::Error for SpanKeyParseError {}
