// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod span_pool;
pub mod trace_utils;
pub mod trace_utils_v1;
pub mod v04;
pub mod v05;
pub mod v1;
pub mod vec_map;

use crate::msgpack_decoder::decode::buffer::{read_string_ref_nomut, Buffer};
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::v05::dict::SharedDict;
use libdd_tinybytes::{Bytes, BytesString};
use serde::Serialize;
use std::borrow::{Borrow, Cow};
use std::fmt;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;

/// A `SpanLink`'s `flags` field reserves bit 31 to mean "a value was explicitly set", separate
/// from the sampling decision carried in the low bits. The sentinel bit distinguishes
/// `flags == 0` (never set) from an explicit decision of `0`, for example a dropped context.
/// Without the sentinel, both cases look identical on the wire.
///
/// Every non-JSON wire format keeps this bit raw, except OTLP. The native v0.4 msgpack format
/// (`msgpack_encoder::v04::span_v04`), the v1 msgpack format, and the native protobuf format
/// (`libdd_trace_protobuf::pb::SpanLink`) all keep the sentinel raw in `flags`. Tracers already
/// send the bit set in these formats. JSON formats and OTLP protobuf must mask this bit before
/// they emit `flags`, because those consumers treat `flags` as the real W3C trace-flags value.
/// The JSON formats are the v0.5 `_dd.span_links` dictionary, agentless JSON, and structured
/// JSON logging.
pub(crate) const SPAN_LINK_FLAGS_SET_SENTINEL: u32 = 1 << 31;

/// Trait representing the requirements for a type to be used as a Span "string" type.
/// Note: Borrow<str> is not required by the derived traits, but allows to access HashMap elements
/// from a static str and check if the string is empty.
pub trait SpanText: Debug + Eq + Hash + Borrow<str> + Serialize + Default + Send {
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

pub trait SpanBytes: Debug + Eq + Hash + Borrow<[u8]> + Serialize + Default + Clone + Send {
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
/// Note: The functions are internal to the msgpack decoder and should not be used directly: they're
/// only exposed here due to the unavailability of min_specialization in stable Rust.
/// Also note that the Clone and PartialEq bounds are only present for tests.
pub trait TraceData: Default + Clone + Debug + PartialEq {
    type Text: SpanText;
    type Bytes: SpanBytes;
}

/// Implemented by the two deserialization modes, parameterized by `'a`, the buffer's data
/// lifetime: `SliceData<'a>` implements it at the slice's real lifetime, `BytesData` at the
/// caller's payload borrow. This ties `Buffer<'a, T>`'s cursor to the lifetime the mode's
/// types can actually prove, so decoded values borrow from the payload honestly.
pub trait DeserializableTraceData<'a>: TraceData {
    /// The borrowed input backing store. This is separate from `TraceData::Bytes`, which is
    /// the representation stored in the decoded payload.
    type Source: ?Sized;

    /// Slices `bytes` bytes at the buffer's cursor, advances the buffer past them, and returns
    /// an owning handle over the sliced range. Returns `None` (without advancing) if fewer
    /// than `bytes` bytes remain.
    fn try_slice_and_advance(buf: &mut Buffer<'a, Self>, bytes: usize) -> Option<Self::Bytes>;

    /// Reads a msgpack string at the buffer's cursor, advances the buffer past it, and returns
    /// the string interned against the buffer's input source.
    fn read_string(buf: &mut Buffer<'a, Self>) -> Result<Self::Text, DecodeError>;

    /// Interns a string harvested while skipping an unrecognized V1 field, for forward
    /// compatibility: a later field may back-reference a string that first appeared inline
    /// inside the skipped value, so skipping must not desync the string table. Both `source`
    /// and `s` borrow from the payload at its honest lifetime `'a`. Returns `None` when the
    /// string cannot be interned (ignored: the V1 encoder never emits strings that would fail
    /// this, so they can never be the target of a later back-reference).
    fn intern_skipped_str(source: &'a Self::Source, s: &'a str) -> Option<Self::Text>;
}

/// TraceData implementation using `Bytes` and `BytesString`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct BytesData;
impl TraceData for BytesData {
    type Text = BytesString;
    type Bytes = Bytes;
}

impl<'a> DeserializableTraceData<'a> for BytesData {
    type Source = Bytes;

    #[inline]
    fn try_slice_and_advance(buf: &mut Buffer<'a, Self>, bytes: usize) -> Option<Bytes> {
        // The remaining bytes are a subslice of the full source payload, so `slice_ref`
        // hands out a refcounted handle over the same allocation.
        let slice = buf.remaining().get(..bytes)?;
        let data = buf.source().slice_ref(slice)?;
        buf.advance(bytes);
        Some(data)
    }

    #[inline]
    fn read_string(buf: &mut Buffer<'a, Self>) -> Result<BytesString, DecodeError> {
        let remaining = buf.remaining();
        let (s, rest) = read_string_ref_nomut(remaining)?;
        // `s` is a subslice of the source payload, so interning is zero-copy and cannot fail.
        let string = BytesString::from_bytes_slice(buf.source(), s);
        buf.advance(remaining.len() - rest.len());
        Ok(string)
    }

    #[inline]
    fn intern_skipped_str(source: &'a Bytes, s: &'a str) -> Option<BytesString> {
        // `try_from_bytes_slice` validates that `s` points into `source` and hands out a
        // handle holding its own refcount on the payload allocation, so the interned string
        // outlives the buffer itself.
        BytesString::try_from_bytes_slice(source, s)
    }
}

/// TraceData implementation using `&str` and `&[u8]`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct SliceData<'a>(PhantomData<&'a u8>);
impl<'a> TraceData for SliceData<'a> {
    type Text = Cow<'a, str>;
    type Bytes = &'a [u8];
}

impl<'a> DeserializableTraceData<'a> for SliceData<'a> {
    type Source = [u8];

    #[inline]
    fn try_slice_and_advance(buf: &mut Buffer<'a, Self>, bytes: usize) -> Option<&'a [u8]> {
        // The cursor views the payload at its real lifetime `'a`.
        let slice = buf.remaining().get(..bytes)?;
        buf.advance(bytes);
        Some(slice)
    }

    #[inline]
    fn read_string(buf: &mut Buffer<'a, Self>) -> Result<Cow<'a, str>, DecodeError> {
        let remaining = buf.remaining();
        let (s, rest) = read_string_ref_nomut(remaining)?;
        buf.advance(remaining.len() - rest.len());
        Ok(Cow::Borrowed(s))
    }

    #[inline]
    fn intern_skipped_str(_source: &'a [u8], s: &'a str) -> Option<Cow<'a, str>> {
        // Nothing to preserve here: `s` already borrows from the source slice at `'a`.
        Some(Cow::Borrowed(s))
    }
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

pub type SharedDictBytes = SharedDict<BytesString>;
