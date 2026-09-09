// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod trace_utils;
pub mod trace_utils_v1;
pub mod v04;
pub mod v05;
pub mod v1;
pub mod vec_map;

use crate::msgpack_decoder::decode::buffer::{read_string_ref_nomut, Buffer, RmpCursor};
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::v05::dict::SharedDict;
use libdd_tinybytes::{Bytes, BytesString};
use rmp::decode;
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
/// Note: The functions are internal to the msgpack decoder and should not be used directly: they're
/// only exposed here due to the unavailability of min_specialization in stable Rust.
/// Also note that the Clone and PartialEq bounds are only present for tests.
pub trait TraceData: Default + Clone + Debug + PartialEq {
    type Text: SpanText;
    type Bytes: SpanBytes;
}

pub trait DeserializableTraceData: TraceData {
    /// `rmp` read cursor over the buffer's payload, at the payload's honest data lifetime
    /// (`decode::Bytes<'a>` for both modes, but the parameter lets each mode pick the lifetime
    /// it can prove: the borrowed slice's `'a` for `SliceData`, the buffer's payload borrow
    /// for `BytesData`).
    type Cursor<'s>: RmpCursor;

    /// Slices `bytes` bytes at the cursor, advances the cursor past them, and returns an
    /// owning handle over the sliced range. Returns `None` (without advancing) if fewer than
    /// `bytes` bytes remain.
    fn try_slice_and_advance(buf: &mut Buffer<'_, Self>, bytes: usize) -> Option<Self::Bytes>;

    /// Reads a msgpack string at the cursor, advances the cursor past it, and returns the
    /// string interned against the buffer's owning handle.
    fn read_string(buf: &mut Buffer<'_, Self>) -> Result<Self::Text, DecodeError>;

    /// Interns a string harvested while skipping an unrecognized V1 field for forward
    /// compatibility. `bytes` is the skipped string's byte range at the payload's honest
    /// lifetime: implementations must derive `Self::Text` from it and `owner` so a refcounted
    /// backing allocation isn't freed out from under the interned string. Returns `None` when
    /// the bytes are not valid UTF-8 (ignored: the V1 encoder never produces such strings, so
    /// they can never be the target of a later back-reference).
    fn intern_skipped_bytes(owner: &Self::Bytes, bytes: &Self::Bytes) -> Option<Self::Text>;
}

/// TraceData implementation using `Bytes` and `BytesString`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct BytesData;
impl TraceData for BytesData {
    type Text = BytesString;
    type Bytes = Bytes;
}

impl DeserializableTraceData for BytesData {
    type Cursor<'s> = decode::Bytes<'s>;

    #[inline]
    fn try_slice_and_advance(buf: &mut Buffer<'_, Self>, bytes: usize) -> Option<Bytes> {
        // The cursor's remaining bytes are a subslice of the full `owner` payload, so
        // `slice_ref` cannot fail: it hands out a refcounted handle over the same range.
        let slice = buf.as_mut_slice().remaining_slice().get(..bytes)?;
        let data = buf.bytes().slice_ref(slice)?;
        buf.advance(bytes);
        Some(data)
    }

    #[inline]
    fn read_string(buf: &mut Buffer<'_, Self>) -> Result<BytesString, DecodeError> {
        let (s, rest) = read_string_ref_nomut(buf.as_mut_slice().remaining_slice())?;
        // `s` is a subslice of the full `owner` payload, so interning cannot fail.
        let string = BytesString::from_bytes_slice(buf.bytes(), s);
        *buf.as_mut_slice() = decode::Bytes::new(rest);
        Ok(string)
    }

    #[inline]
    fn intern_skipped_bytes(owner: &Bytes, bytes: &Bytes) -> Option<BytesString> {
        // The lifetime of `s` doesn't matter: `from_bytes_slice` validates containment and
        // hands out a handle holding its own refcount on the payload allocation.
        let s = std::str::from_utf8(bytes.as_ref()).ok()?;
        Some(BytesString::from_bytes_slice(owner, s))
    }
}

/// TraceData implementation using `&str` and `&[u8]`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct SliceData<'a>(PhantomData<&'a u8>);
impl<'a> TraceData for SliceData<'a> {
    type Text = Cow<'a, str>;
    type Bytes = &'a [u8];
}

impl<'a> DeserializableTraceData for SliceData<'a> {
    type Cursor<'s> = decode::Bytes<'a>;

    #[inline]
    fn try_slice_and_advance(buf: &mut Buffer<'_, Self>, bytes: usize) -> Option<&'a [u8]> {
        // The cursor views the payload at its real lifetime `'a`.
        let slice = buf.as_mut_slice().remaining_slice().get(..bytes)?;
        buf.advance(bytes);
        Some(slice)
    }

    #[inline]
    fn read_string(buf: &mut Buffer<'_, Self>) -> Result<Cow<'a, str>, DecodeError> {
        let (s, rest) = read_string_ref_nomut(buf.as_mut_slice().remaining_slice())?;
        *buf.as_mut_slice() = decode::Bytes::new(rest);
        Ok(Cow::Borrowed(s))
    }

    #[inline]
    fn intern_skipped_bytes(_owner: &&'a [u8], bytes: &&'a [u8]) -> Option<Cow<'a, str>> {
        // No refcounted allocation to preserve here: the bytes borrow from a plain slice
        // the caller owns for `'a`.
        std::str::from_utf8(bytes).ok().map(Cow::Borrowed)
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
