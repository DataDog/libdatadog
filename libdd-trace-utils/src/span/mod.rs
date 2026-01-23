// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod trace_utils;
pub mod v04;
pub mod v05;

use crate::msgpack_decoder::decode::buffer::read_string_ref_nomut;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::v05::dict::SharedDict;
use libdd_tinybytes::{Bytes, BytesString};
use serde::Serialize;
use std::borrow::Borrow;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::{fmt, ptr};

/// Trait representing the requirements for a type to be used as a Span "string" type.
/// Note: Borrow<str> is not required by the derived traits, but allows to access HashMap elements
/// from a static str and check if the string is empty.
pub trait SpanText: Debug + Eq + Hash + Borrow<str> + Serialize + Default {
    fn from_static_str(value: &'static str) -> Self;
}

impl SpanText for &str {
    fn from_static_str(value: &'static str) -> Self {
        value
    }
}

impl SpanText for BytesString {
    fn from_static_str(value: &'static str) -> Self {
        BytesString::from_static(value)
    }
}

pub trait SpanBytes: Debug + Eq + Hash + Borrow<[u8]> + Serialize + Default {
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
    fn get_mut_slice(buf: &mut Self::Bytes) -> &mut &'static [u8];

    fn try_slice_and_advance(buf: &mut Self::Bytes, bytes: usize) -> Option<Self::Bytes>;

    fn read_string(buf: &mut Self::Bytes) -> Result<Self::Text, DecodeError>;
}

/// TraceData implementation using `Bytes` and `BytesString`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct BytesData;
impl TraceData for BytesData {
    type Text = BytesString;
    type Bytes = Bytes;
}

impl DeserializableTraceData for BytesData {
    #[inline]
    fn get_mut_slice(buf: &mut Bytes) -> &mut &'static [u8] {
        // SAFETY: Bytes has the same layout
        unsafe { std::mem::transmute::<&mut Bytes, &mut &[u8]>(buf) }
    }

    #[inline]
    fn try_slice_and_advance(buf: &mut Bytes, bytes: usize) -> Option<Bytes> {
        let data = buf.slice_ref(&buf[0..bytes])?;
        unsafe {
            // SAFETY: forwarding the buffer requires that buf is borrowed from static.
            let (ptr, len, underlying) = ptr::read(buf).into_raw();
            ptr::write(
                buf,
                Bytes::from_raw(ptr.add(bytes), len - bytes, underlying),
            );
        }
        Some(data)
    }

    #[inline]
    fn read_string(buf: &mut Bytes) -> Result<BytesString, DecodeError> {
        // Note: we need to pass a &'static lifetime here, otherwise it'll complain
        let (str, newbuf) = read_string_ref_nomut(buf.as_ref())?;
        let string = BytesString::from_bytes_slice(buf, str);
        unsafe {
            // SAFETY: forwarding the buffer requires that buf is borrowed from static.
            let (_, _, underlying) = ptr::read(buf).into_raw();
            let new = Bytes::from_raw(
                NonNull::new_unchecked(newbuf.as_ptr() as *mut _),
                newbuf.len(),
                underlying,
            );
            ptr::write(buf, new);
        }
        Ok(string)
    }
}

/// TraceData implementation using `&str` and `&[u8]`.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
pub struct SliceData<'a>(PhantomData<&'a u8>);
impl<'a> TraceData for SliceData<'a> {
    type Text = &'a str;
    type Bytes = &'a [u8];
}

impl<'a> DeserializableTraceData for SliceData<'a> {
    #[inline]
    fn get_mut_slice<'b>(buf: &'b mut Self::Bytes) -> &'b mut &'static [u8] {
        unsafe { std::mem::transmute::<&'b mut &[u8], &'b mut &'static [u8]>(buf) }
    }

    #[inline]
    fn try_slice_and_advance(buf: &mut &'a [u8], bytes: usize) -> Option<&'a [u8]> {
        let slice = buf.get(0..bytes)?;
        *buf = &buf[bytes..];
        Some(slice)
    }

    #[inline]
    fn read_string(buf: &mut &'a [u8]) -> Result<&'a str, DecodeError> {
        read_string_ref_nomut(buf).map(|(str, newbuf)| {
            *buf = newbuf;
            str
        })
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
