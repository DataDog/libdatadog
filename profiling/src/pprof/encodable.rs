// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use prost::encoding::{WireType, MAX_TAG, MIN_TAG};
use prost::*;
use std::ops::Range;

pub use super::proto::{Function, Line};

/// Represents something that converts to the in-wire LEN type.
pub trait LenEncodable {
    fn encoded_len(&self) -> usize;

    /// Encodes the value into the in-wire protobuf format. Returns the range
    /// of bytes which could be used to deduplicate two values of the same
    /// message type (e.g., excludes the Mapping.id field in Mapping).
    ///
    /// # Safety
    /// The buffer needs to have at least [Self::encoded_len] free bytes, and
    /// the message being encoded needs to match this number of bytes as well.
    unsafe fn encode(&self, buf: &mut Vec<u8>) -> Range<u32>;
}

/// Encodes the value into the in-wire protobuf format, including the tag and
/// length prefix.
///
/// # Safety
/// There must be space for the tag, length delimiter, and the message.
#[cfg_attr(debug_assertions, track_caller)]
pub unsafe fn encode_with_tag_unchecked(
    encodable: &impl LenEncodable,
    tag: u32,
    encoded_len: usize,
    buf: &mut Vec<u8>,
) -> Range<u32> {
    debug_assert!(encoded_len == encodable.encoded_len());
    encode_key(tag, WireType::LengthDelimited, buf);
    encode_varint(encoded_len as u64, buf);
    encodable.encode(buf)
}

/// Encodes the value into the in-wire protobuf format, including the tag and
/// length prefix. This uses [Vec::try_reserve] to ensure there's enough
/// memory for the message.
///
/// Returns the range of bytes which could be used to deduplicate two values
/// of the same message type (e.g., excludes the Mapping.id field in Mapping).
#[inline]
pub fn try_encode_with_tag(
    encodable: &impl LenEncodable,
    tag: u32,
    buf: &mut Vec<u8>,
) -> Result<Range<u32>, EncodeError> {
    let encoded_len = encodable.encoded_len();
    let required =
        encoding::key_len(tag) + encoding::encoded_len_varint(encoded_len as u64) + encoded_len;
    if buf.len() + required > PROTOBUF_MAX_MESSAGE_BYTES {
        let len = buf.len();
        Err(EncodeError::TooLarge { len, required })
    } else if let Err(_err) = buf.try_reserve(required) {
        Err(EncodeError::Oom {
            required,
            remaining: required,
        })
    } else {
        // SAFETY: the proper number of bytes were reserved to encode the tag,
        // length, and the message.
        Ok(unsafe { encode_with_tag_unchecked(encodable, tag, encoded_len, buf) })
    }
}

/// Represents a Mapping for pprof, with some fields omitted. We don't
/// currently use those fields at all in our end-user APIs, so we omit them
/// here to save space/bytes/cpu ops. Since they'd default to the zero repr
/// anyway, they can just be omitted from the protobuf in-wire.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Mapping {
    pub id: u64,           // 1
    pub memory_start: u64, // 2
    pub memory_limit: u64, // 3
    pub file_offset: u64,  // 4
    pub filename: i64,     // 5
    pub build_id: i64,     // 6
}

/// Represents a Location for pprof. This borrows the slice for the lines
/// because it's not meant to be long-lived--you create one, serialize it,
/// and throw the struct away.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Location {
    pub id: u64,         // 1
    pub mapping_id: u64, // 2
    pub address: u64,    // 3
    pub line: Line,      // 4
}

impl From<Mapping> for crate::pprof::proto::Mapping {
    fn from(value: Mapping) -> Self {
        Self {
            id: value.id,
            memory_start: value.memory_start,
            memory_limit: value.memory_limit,
            file_offset: value.file_offset,
            filename: value.filename,
            build_id: value.build_id,
            has_functions: false,
            has_filenames: false,
            has_line_numbers: false,
            has_inline_frames: false,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum EncodeError {
    #[error("protobuf messages need to fit in 2 GiB and the buffer is currently {len} bytes and needed {required} more")]
    TooLarge { len: usize, required: usize },

    #[error("protobuf message needed {required} bytes, only {remaining} remaining")]
    Oom { required: usize, remaining: usize },
}

/// Protobuf messages need to fit in 2 GiB, which is plenty for profiling.
const PROTOBUF_MAX_MESSAGE_BYTES: usize = (u32::MAX as usize) - 1;

///  # Safety
/// The encoded_len must match the number of bytes needed to encode the value,
/// and the buffer must have at least this number of bytes of unused capacity.
#[inline]
#[cfg_attr(debug_assertions, track_caller)]
unsafe fn encode_varint_with_tag_with_zero_opt(tag: u32, value: u64, buf: &mut Vec<u8>) {
    if value != 0 {
        // SAFETY: guarded by function's safety conditions.
        unsafe { encode_varint_with_tag(tag, value, buf) };
    }
}

#[inline]
fn encoded_len_u64_with_zero_opt(tag: u32, num: u64) -> usize {
    if num != 0 {
        encoded_len_u64_without_zero_opt(tag, num)
    } else {
        0
    }
}

#[inline]
fn encoded_len_u64_without_zero_opt(tag: u32, num: u64) -> usize {
    encoding::uint64::encoded_len(tag, &num)
}

#[inline]
fn encoded_len_i64_with_zero_opt(tag: u32, num: i64) -> usize {
    if num != 0 {
        encoded_len_i64_without_zero_opt(tag, num)
    } else {
        0
    }
}

#[inline]
fn encoded_len_i64_without_zero_opt(tag: u32, num: i64) -> usize {
    encoded_len_u64_without_zero_opt(tag, num as u64)
}

/// # Safety
/// Assumes there is enough capacity to write the varint.
#[inline]
unsafe fn encode_varint(mut value: u64, buf: &mut Vec<u8>) {
    loop {
        if buf.len() == buf.capacity() {
            // SAFETY: the try_reserve above prevents this branch.
            unsafe { std::hint::unreachable_unchecked() }
        }
        buf.push(if value < 0x80 {
            value as u8
        } else {
            ((value & 0x7F) | 0x80) as u8
        });
        if value < 0x80 {
            break;
        }
        value >>= 7;
    }
}

/// # Safety
/// Assumes there is enough capacity to write the key.
#[inline]
#[cfg_attr(debug_assertions, track_caller)]
unsafe fn encode_key(tag: u32, wire_type: WireType, buf: &mut Vec<u8>) {
    debug_assert!((MIN_TAG..=MAX_TAG).contains(&tag));
    let key = (tag << 3) | wire_type as u32;
    // SAFETY: see function's safety conditions.
    unsafe { encode_varint(u64::from(key), buf) };
}

///  # Safety
/// The encoded_len must match the number of bytes needed to encode the value,
/// and the buffer must have at least this number of bytes of unused capacity.
#[cfg_attr(debug_assertions, track_caller)]
unsafe fn encode_varint_with_tag(tag: u32, value: u64, buf: &mut Vec<u8>) {
    debug_assert!(encoded_len_u64_without_zero_opt(tag, value) <= buf.capacity() - buf.len());
    encode_key(tag, WireType::Varint, buf);
    encode_varint(value, buf);
}

impl LenEncodable for Mapping {
    fn encoded_len(&self) -> usize {
        // Without zero-opt because it's inherently non-zero.
        let id = encoded_len_u64_without_zero_opt(1u32, self.id);
        let memory_start = encoded_len_u64_with_zero_opt(2u32, self.memory_start);
        let memory_limit = encoded_len_u64_with_zero_opt(3u32, self.memory_limit);
        let file_offset = encoded_len_u64_with_zero_opt(4u32, self.file_offset);
        // Without zero-opt because we use it for a unique prefix.
        let filename = encoded_len_i64_without_zero_opt(5u32, self.filename);
        let build_id = encoded_len_i64_with_zero_opt(6u32, self.build_id);

        id + memory_start + memory_limit + file_offset + filename + build_id
    }

    unsafe fn encode(&self, buf: &mut Vec<u8>) -> Range<u32> {
        // The id comes off first so we can slice it off for byte comparisons.
        // SAFETY: size is guarded above, and the given lens match the needed
        // number of bytes.
        unsafe {
            encode_varint_with_tag(1u32, self.id, buf);
            // For Mapping, we use filename (tag 5) as our unique prefix.
            let start = buf.len() as u32;
            encode_varint_with_tag(5u32, self.filename as u64, buf);

            encode_varint_with_tag_with_zero_opt(2u32, self.memory_start, buf);
            encode_varint_with_tag_with_zero_opt(3u32, self.memory_limit, buf);
            encode_varint_with_tag_with_zero_opt(4u32, self.file_offset, buf);
            // tag 5 (filename) was already encoded
            encode_varint_with_tag_with_zero_opt(6u32, self.build_id as u64, buf);
            let end = buf.len() as u32;

            Range { start, end }
        }
    }
}

impl LenEncodable for Function {
    fn encoded_len(&self) -> usize {
        // Without zero-opt because it's inherently non-zero.
        let id = encoded_len_u64_without_zero_opt(1u32, self.id);
        // Without zero-opt because we use it for a unique prefix.
        let name = encoded_len_i64_without_zero_opt(2u32, self.name);
        let system_name = encoded_len_i64_with_zero_opt(3u32, self.system_name);
        let filename = encoded_len_i64_with_zero_opt(4u32, self.filename);
        let start_line = encoded_len_i64_with_zero_opt(5u32, self.start_line);

        id + name + system_name + filename + start_line
    }

    unsafe fn encode(&self, buf: &mut Vec<u8>) -> Range<u32> {
        // The id comes off first so we can slice it off for byte comparisons.
        // SAFETY: size is guarded above, and the given lens match the needed
        // number of bytes.
        unsafe {
            encode_varint_with_tag(1u32, self.id, buf);

            // For Function, we use name (tag 2) as our unique prefix.
            let start = buf.len() as u32;
            encode_varint_with_tag(2u32, self.name as u64, buf);
            encode_varint_with_tag_with_zero_opt(3u32, self.system_name as u64, buf);
            encode_varint_with_tag_with_zero_opt(4u32, self.filename as u64, buf);
            encode_varint_with_tag_with_zero_opt(5u32, self.start_line as u64, buf);

            let end = buf.len() as u32;

            Range { start, end }
        }
    }
}

impl LenEncodable for Location {
    fn encoded_len(&self) -> usize {
        // Without zero-opt because it's inherently non-zero.
        let id = encoded_len_u64_without_zero_opt(1u32, self.id);
        let mapping_id = encoded_len_u64_with_zero_opt(2u32, self.mapping_id);
        let address = encoded_len_u64_with_zero_opt(3u32, self.address);
        let line = {
            let item = Message::encoded_len(&self.line);
            let tag = encoding::key_len(4u32);
            let len_prefix = encoding::encoded_len_varint(item as u64);
            tag + len_prefix + item
        };

        id + mapping_id + address + line
    }

    unsafe fn encode(&self, buf: &mut Vec<u8>) -> Range<u32> {
        // The id comes off first so we can slice it off for byte comparisons.
        // SAFETY: size is guarded above, and the given lens match the needed
        // number of bytes.
        unsafe {
            encode_varint_with_tag(1u32, self.id, buf);

            // For Location, we use line (tag 4) as our unique prefix.
            let start = buf.len() as u32;

            encode_key(4u32, WireType::LengthDelimited, buf);
            encode_varint(Message::encoded_len(&self.line) as u64, buf);
            // Can ignore errors, as we've reserved enough capacity prior.
            _ = Message::encode(&self.line, buf);

            encode_varint_with_tag_with_zero_opt(2u32, self.mapping_id, buf);
            encode_varint_with_tag_with_zero_opt(3u32, self.address, buf);

            let end = buf.len() as u32;

            Range { start, end }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mapping() {
        let cases = [
            Mapping {
                id: 0,
                memory_start: 0,
                memory_limit: 0,
                file_offset: 0,
                filename: 0,
                build_id: 0,
            },
            Mapping {
                id: 11,
                memory_start: 12,
                memory_limit: 13,
                file_offset: 14,
                filename: 15,
                build_id: 16,
            },
            Mapping {
                id: u64::MAX,
                memory_start: u64::MAX,
                memory_limit: u64::MAX,
                file_offset: u64::MAX,
                filename: i64::MAX,
                build_id: i64::MAX,
            },
        ];
        for before in cases {
            use crate::pprof::proto::Mapping as PprofMapping;
            let required = {
                let ddog = LenEncodable::encoded_len(&before);
                let prost = Message::encoded_len(&(PprofMapping::from(before)));
                // We skip the zero-encoding optimization for some items to
                // ensure a unique prefix, so we should be equal or bigger
                // than the prost version depending on the provided values,
                // but we should never be smaller.
                assert!(ddog >= prost);
                ddog
            };
            let mut buf = Vec::with_capacity(required);
            // SAFETY: correct size, plus buffer has reserved capacity.
            let _range = unsafe { LenEncodable::encode(&before, &mut buf) };
            // We exactly request the number of bytes, this should match.
            let remaining = buf.capacity() - buf.len();
            assert_eq!(
                0, remaining,
                "{remaining} bytes unused, encoded len may be too large or encoder may be wrong for {before:?}",
            );

            let after = PprofMapping::decode(buf.as_slice()).unwrap();
            assert_eq!(before.id, after.id);
            assert_eq!(before.memory_start, after.memory_start);
            assert_eq!(before.memory_limit, after.memory_limit);
            assert_eq!(before.file_offset, after.file_offset);
            assert_eq!(before.filename, after.filename);
            assert_eq!(before.build_id, after.build_id);
        }
    }

    #[test]
    fn test_function() {
        let cases = [
            Function {
                id: 0,
                name: 0,
                system_name: 0,
                filename: 0,
                start_line: 0,
            },
            Function {
                id: 11,
                name: 12,
                system_name: 13,
                filename: 14,
                start_line: 15,
            },
            Function {
                id: 65536,
                name: 32768,
                system_name: 16384,
                filename: 8192,
                start_line: 4096,
            },
            Function {
                id: u64::MAX,
                name: i64::MAX,
                system_name: i64::MAX,
                filename: i64::MAX,
                start_line: i64::MAX,
            },
        ];
        for before in cases {
            let required = {
                let ddog = LenEncodable::encoded_len(&before);
                let prost = Message::encoded_len(&before);
                // We skip the zero-encoding optimization for some items to
                // ensure a unique prefix, so we should be equal or bigger
                // than the prost version depending on the provided values,
                // but we should never be smaller.
                assert!(ddog >= prost);
                ddog
            };
            let mut buf = Vec::with_capacity(required);
            // SAFETY: correct size, plus buffer has reserved capacity.
            let _range = unsafe { LenEncodable::encode(&before, &mut buf) };
            // We exactly request the number of bytes, this should match.
            let remaining = buf.capacity() - buf.len();
            assert_eq!(
                0, remaining,
                "{remaining} bytes unused, encoded len may be too large or encoder may be wrong for {before:?}",
            );

            let after = Function::decode(buf.as_slice()).unwrap();
            assert_eq!(before, after);
            // The implementation of Eq ignores the id field so we check it too.
            assert_eq!(before.id, after.id);
        }
    }

    #[test]
    fn test_location() {
        let before = Location {
            id: 1,
            mapping_id: 2,
            address: 3,
            line: Line {
                function_id: 4,
                line: 5,
            },
        };

        let len = LenEncodable::encoded_len(&before);
        let mut buf = Vec::with_capacity(len);

        // SAFETY: correct size, plus buffer has reserved capacity.
        let _range = unsafe { LenEncodable::encode(&before, &mut buf) };

        // We exactly request the number of bytes, this should match.
        let remaining = buf.capacity() - buf.len();
        assert_eq!(
            0, remaining,
            "{remaining} bytes unused, encoded len may be too large or encoder may be wrong for {before:?}",
        );

        let after = crate::pprof::Location::decode(buf.as_slice()).unwrap();
        // different reprs, have to go piece by piece.
        assert_eq!(before.id, after.id);
        assert_eq!(before.mapping_id, after.mapping_id);
        assert_eq!(before.address, after.address);

        assert_eq!(&before.line, after.lines.first().unwrap());
    }
}
