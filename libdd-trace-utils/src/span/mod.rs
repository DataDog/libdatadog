// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod trace_utils;
pub mod trace_utils_v1;
pub mod v05;

use crate::msgpack_decoder::decode::buffer::read_string_ref_nomut;
use crate::msgpack_decoder::decode::error::DecodeError;
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_types::span::{BytesData, SliceData, TraceData};
use std::borrow::Cow;
use std::ptr::{self, NonNull};

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

/// Decoder-specific trait: augments TraceData with decoder operations for msgpack decoding.
pub trait DeserializableTraceData: TraceData {
    fn get_mut_slice(buf: &mut Self::Bytes) -> &mut &'static [u8];

    fn try_slice_and_advance(buf: &mut Self::Bytes, bytes: usize) -> Option<Self::Bytes>;

    fn read_string(buf: &mut Self::Bytes) -> Result<Self::Text, DecodeError>;

    /// Interns a string found while walking a value through `get_mut_slice`'s lied `'static`
    /// view (e.g. skipping an unrecognized V1 field for forward compatibility). `s` really
    /// borrows from `owner`'s memory, not `'static`: implementations must derive `Self::Text`
    /// from `owner` itself rather than trusting that lifetime, so a refcounted backing
    /// allocation isn't freed out from under the interned string.
    fn intern_skipped_str(owner: &Self::Bytes, s: &'static str) -> Self::Text;
}

impl DeserializableTraceData for BytesData {
    #[inline]
    fn get_mut_slice(buf: &mut Bytes) -> &mut &'static [u8] {
        // SAFETY: Bytes has the same layout
        unsafe { std::mem::transmute::<&mut Bytes, &mut &[u8]>(buf) }
    }

    #[inline]
    fn try_slice_and_advance(buf: &mut Bytes, bytes: usize) -> Option<Bytes> {
        if bytes > buf.len() {
            return None;
        }
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

    #[inline]
    fn intern_skipped_str(owner: &Bytes, s: &'static str) -> BytesString {
        BytesString::from_bytes_slice(owner, s)
    }
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
    fn read_string(buf: &mut &'a [u8]) -> Result<Cow<'a, str>, DecodeError> {
        read_string_ref_nomut(buf).map(|(str, newbuf)| {
            *buf = newbuf;
            Cow::Borrowed(str)
        })
    }

    #[inline]
    fn intern_skipped_str(_owner: &&'a [u8], s: &'static str) -> Cow<'a, str> {
        // No refcounted allocation to preserve here: `s` borrows from a plain slice the
        // caller owns for `'a`, and a `'static` reference is always a valid `'a` reference.
        Cow::Borrowed(s)
    }
}
