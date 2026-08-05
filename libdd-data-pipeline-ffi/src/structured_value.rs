// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
#[cfg(all(feature = "catch_panic", panic = "unwind"))]
use crate::gen_error;
use libdd_common_ffi::slice::{AsBytes, ByteSlice, Slice};
use std::ptr::NonNull;

const DDOG_TRACER_VALUE_NIL: u8 = 0;
const DDOG_TRACER_VALUE_BOOL: u8 = 1;
const DDOG_TRACER_VALUE_I64: u8 = 2;
const DDOG_TRACER_VALUE_U64: u8 = 3;
const DDOG_TRACER_VALUE_F64: u8 = 4;
const DDOG_TRACER_VALUE_STRING: u8 = 5;
const DDOG_TRACER_VALUE_BINARY: u8 = 6;
const DDOG_TRACER_VALUE_ARRAY: u8 = 7;
const DDOG_TRACER_VALUE_MAP: u8 = 8;

const MAX_DEPTH: u32 = 64;

/// One value in a flat preorder representation of a structured value.
///
/// Scalar tokens use the corresponding scalar field. String and binary tokens
/// use `bytes`. Array and map tokens use `child_count`; a map is followed by
/// two values per entry (key, then value). All other fields must be ignored.
/// Integer `kind` constants are used instead of a C enum so malformed tags can
/// be rejected without constructing an invalid Rust enum discriminant.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TracerValueToken<'a> {
    pub kind: u8,
    pub bool_value: u8,
    pub child_count: u32,
    pub i64_value: i64,
    pub u64_value: u64,
    pub f64_value: f64,
    pub bytes: ByteSlice<'a>,
}

/// Opaque owned MessagePack blob produced from structured-value tokens.
pub struct TracerEncodedValue(Vec<u8>);

fn invalid_input(message: &str) -> Box<ExporterError> {
    Box::new(ExporterError::new(ErrorCode::InvalidInput, message))
}

fn write_len(
    output: &mut Vec<u8>,
    len: u32,
    fix_base: u8,
    fix_max: u32,
    marker16: u8,
    marker32: u8,
) {
    if len <= fix_max {
        output.push(fix_base | len as u8);
    } else if u16::try_from(len).is_ok() {
        output.push(marker16);
        output.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        output.push(marker32);
        output.extend_from_slice(&len.to_be_bytes());
    }
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    if value <= 0x7f {
        output.push(value as u8);
    } else if value <= u8::MAX as u64 {
        output.extend_from_slice(&[0xcc, value as u8]);
    } else if value <= u16::MAX as u64 {
        output.push(0xcd);
        output.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= u32::MAX as u64 {
        output.push(0xce);
        output.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        output.push(0xcf);
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn write_i64(output: &mut Vec<u8>, value: i64) {
    if value >= 0 {
        write_u64(output, value as u64);
    } else if value >= -32 {
        output.push(value as i8 as u8);
    } else if value >= i8::MIN as i64 {
        output.extend_from_slice(&[0xd0, value as i8 as u8]);
    } else if value >= i16::MIN as i64 {
        output.push(0xd1);
        output.extend_from_slice(&(value as i16).to_be_bytes());
    } else if value >= i32::MIN as i64 {
        output.push(0xd2);
        output.extend_from_slice(&(value as i32).to_be_bytes());
    } else {
        output.push(0xd3);
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn write_bytes(output: &mut Vec<u8>, bytes: &[u8], string: bool) -> Result<(), Box<ExporterError>> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| invalid_input("structured value byte string exceeds u32::MAX"))?;
    if string {
        std::str::from_utf8(bytes)
            .map_err(|_| invalid_input("structured value string is not valid UTF-8"))?;
        if len <= 31 {
            output.push(0xa0 | len as u8);
        } else if len <= u8::MAX as u32 {
            output.extend_from_slice(&[0xd9, len as u8]);
        } else if len <= u16::MAX as u32 {
            output.push(0xda);
            output.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            output.push(0xdb);
            output.extend_from_slice(&len.to_be_bytes());
        }
    } else if len <= u8::MAX as u32 {
        output.extend_from_slice(&[0xc4, len as u8]);
    } else if len <= u16::MAX as u32 {
        output.push(0xc5);
        output.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        output.push(0xc6);
        output.extend_from_slice(&len.to_be_bytes());
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn encode_one(
    tokens: &[TracerValueToken<'_>],
    index: &mut usize,
    depth: u32,
    output: &mut Vec<u8>,
) -> Result<(), Box<ExporterError>> {
    let token = tokens
        .get(*index)
        .ok_or_else(|| invalid_input("structured value container is missing child tokens"))?;
    *index += 1;

    match token.kind {
        DDOG_TRACER_VALUE_NIL => output.push(0xc0),
        DDOG_TRACER_VALUE_BOOL => match token.bool_value {
            0 => output.push(0xc2),
            1 => output.push(0xc3),
            _ => return Err(invalid_input("structured value boolean must be 0 or 1")),
        },
        DDOG_TRACER_VALUE_I64 => write_i64(output, token.i64_value),
        DDOG_TRACER_VALUE_U64 => write_u64(output, token.u64_value),
        DDOG_TRACER_VALUE_F64 => {
            output.push(0xcb);
            output.extend_from_slice(&token.f64_value.to_be_bytes());
        }
        DDOG_TRACER_VALUE_STRING | DDOG_TRACER_VALUE_BINARY => {
            let bytes = token
                .bytes
                .try_as_bytes()
                .map_err(|_| invalid_input("structured value contains an invalid byte slice"))?;
            write_bytes(output, bytes, token.kind == DDOG_TRACER_VALUE_STRING)?;
        }
        DDOG_TRACER_VALUE_ARRAY | DDOG_TRACER_VALUE_MAP => {
            if depth >= MAX_DEPTH {
                return Err(invalid_input(
                    "structured value exceeds maximum depth of 64",
                ));
            }
            let values = if token.kind == DDOG_TRACER_VALUE_MAP {
                token
                    .child_count
                    .checked_mul(2)
                    .ok_or_else(|| invalid_input("structured value map child count overflows"))?
            } else {
                token.child_count
            };
            if token.kind == DDOG_TRACER_VALUE_ARRAY {
                write_len(output, token.child_count, 0x90, 15, 0xdc, 0xdd);
            } else {
                write_len(output, token.child_count, 0x80, 15, 0xde, 0xdf);
            }
            for _ in 0..values {
                encode_one(tokens, index, depth + 1, output)?;
            }
        }
        _ => {
            return Err(invalid_input(
                "structured value contains an unknown token kind",
            ))
        }
    }
    Ok(())
}

pub(crate) fn encode_value(
    tokens: Slice<TracerValueToken<'_>>,
) -> Result<Vec<u8>, Box<ExporterError>> {
    let tokens = tokens
        .try_as_slice()
        .map_err(|_| invalid_input("structured value token slice is invalid"))?;
    if tokens.is_empty() {
        return Err(invalid_input("structured value token slice is empty"));
    }
    let mut output = Vec::new();
    let mut index = 0;
    encode_one(tokens, &mut index, 0, &mut output)?;
    if index != tokens.len() {
        return Err(invalid_input("structured value has trailing tokens"));
    }
    Ok(output)
}

/// Encode one flat preorder structured value as an owned MessagePack blob.
///
/// On success, `out_handle` receives an owned blob that must be freed with
/// [`ddog_tracer_encoded_value_free`]. The input is fully validated and must
/// contain exactly one value. Token byte slices are borrowed only for this
/// synchronous call; the returned blob does not retain them.
///
/// # Safety
///
/// `tokens` and every byte slice referenced by its tokens must remain valid for
/// this call. `out_handle` must point to writable memory for a
/// `Box<TracerEncodedValue>`.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_encode_value(
    tokens: Slice<TracerValueToken<'_>>,
    out_handle: NonNull<Box<TracerEncodedValue>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        {
            let inner = || -> Result<(), Box<ExporterError>> {
                let output = encode_value(tokens)?;
                out_handle
                    .as_ptr()
                    .write(Box::new(TracerEncodedValue(output)));
                Ok(())
            };
            inner().err()
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Borrow the bytes in an encoded value. The slice is valid until the blob is
/// freed.
#[no_mangle]
pub extern "C" fn ddog_tracer_encoded_value_as_slice(
    value: Option<&TracerEncodedValue>,
) -> ByteSlice<'_> {
    value
        .map(|value| ByteSlice::from(value.0.as_slice()))
        .unwrap_or_default()
}

/// Free an encoded structured value and its bytes.
#[no_mangle]
pub extern "C" fn ddog_tracer_encoded_value_free(value: Option<Box<TracerEncodedValue>>) {
    drop(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn token<'a>(kind: u8) -> TracerValueToken<'a> {
        TracerValueToken {
            kind,
            bool_value: 0,
            child_count: 0,
            i64_value: 0,
            u64_value: 0,
            f64_value: 0.0,
            bytes: ByteSlice::empty(),
        }
    }

    unsafe fn encode(tokens: &[TracerValueToken<'_>]) -> Result<Vec<u8>, Box<ExporterError>> {
        let blob = encode_blob(tokens)?;
        let bytes = ddog_tracer_encoded_value_as_slice(Some(&blob))
            .as_bytes()
            .to_vec();
        ddog_tracer_encoded_value_free(Some(blob));
        Ok(bytes)
    }

    unsafe fn encode_blob(
        tokens: &[TracerValueToken<'_>],
    ) -> Result<Box<TracerEncodedValue>, Box<ExporterError>> {
        let mut handle = MaybeUninit::<Box<TracerEncodedValue>>::uninit();
        let out = NonNull::new(handle.as_mut_ptr()).unwrap();
        if let Some(error) = ddog_tracer_encode_value(Slice::from(tokens), out) {
            return Err(error);
        }
        Ok(handle.assume_init())
    }

    #[test]
    fn encodes_all_supported_values() {
        let mut map = token(DDOG_TRACER_VALUE_MAP);
        map.child_count = 7;
        let mut bools = token(DDOG_TRACER_VALUE_ARRAY);
        bools.child_count = 2;
        let mut false_token = token(DDOG_TRACER_VALUE_BOOL);
        false_token.bool_value = 0;
        let mut true_token = token(DDOG_TRACER_VALUE_BOOL);
        true_token.bool_value = 1;
        let mut signed = token(DDOG_TRACER_VALUE_I64);
        signed.i64_value = i64::MIN;
        let mut unsigned = token(DDOG_TRACER_VALUE_U64);
        unsigned.u64_value = u64::MAX;
        let mut float = token(DDOG_TRACER_VALUE_F64);
        float.f64_value = 1.25;
        let mut binary = token(DDOG_TRACER_VALUE_BINARY);
        binary.bytes = ByteSlice::from(&b"\0\xff"[..]);

        let strings = [
            "nil", "bools", "signed", "unsigned", "float", "string", "binary", "hello",
        ];
        let string_tokens: Vec<_> = strings
            .iter()
            .map(|value| {
                let mut t = token(DDOG_TRACER_VALUE_STRING);
                t.bytes = ByteSlice::from(value.as_bytes());
                t
            })
            .collect();
        let tokens = [
            map,
            string_tokens[0],
            token(DDOG_TRACER_VALUE_NIL),
            string_tokens[1],
            bools,
            false_token,
            true_token,
            string_tokens[2],
            signed,
            string_tokens[3],
            unsigned,
            string_tokens[4],
            float,
            string_tokens[5],
            string_tokens[7],
            string_tokens[6],
            binary,
        ];

        let encoded = unsafe { encode(&tokens).unwrap() };
        let mut expected = vec![
            0x87, 0xa3, b'n', b'i', b'l', 0xc0, 0xa5, b'b', b'o', b'o', b'l', b's', 0x92, 0xc2,
            0xc3, 0xa6, b's', b'i', b'g', b'n', b'e', b'd', 0xd3,
        ];
        expected.extend_from_slice(&i64::MIN.to_be_bytes());
        expected.extend_from_slice(&[0xa8, b'u', b'n', b's', b'i', b'g', b'n', b'e', b'd', 0xcf]);
        expected.extend_from_slice(&u64::MAX.to_be_bytes());
        expected.extend_from_slice(&[0xa5, b'f', b'l', b'o', b'a', b't', 0xcb]);
        expected.extend_from_slice(&1.25f64.to_be_bytes());
        expected.extend_from_slice(&[
            0xa6, b's', b't', b'r', b'i', b'n', b'g', 0xa5, b'h', b'e', b'l', b'l', b'o', 0xa6,
            b'b', b'i', b'n', b'a', b'r', b'y', 0xc4, 0x02, 0x00, 0xff,
        ]);
        assert_eq!(encoded, expected);
    }

    #[test]
    fn rejects_empty_missing_and_trailing_tokens() {
        assert!(unsafe { encode(&[]) }.is_err());

        let mut array = token(DDOG_TRACER_VALUE_ARRAY);
        array.child_count = 1;
        assert!(unsafe { encode(&[array]) }.is_err());
        assert!(
            unsafe { encode(&[token(DDOG_TRACER_VALUE_NIL), token(DDOG_TRACER_VALUE_NIL),]) }
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_kinds_booleans_and_utf8() {
        assert!(unsafe { encode(&[token(255)]) }.is_err());

        let mut boolean = token(DDOG_TRACER_VALUE_BOOL);
        boolean.bool_value = 2;
        assert!(unsafe { encode(&[boolean]) }.is_err());

        let mut string = token(DDOG_TRACER_VALUE_STRING);
        string.bytes = ByteSlice::from(&b"\xff"[..]);
        assert!(unsafe { encode(&[string]) }.is_err());
    }

    #[test]
    fn rejects_excessive_depth_and_map_count_overflow() {
        let mut array = token(DDOG_TRACER_VALUE_ARRAY);
        array.child_count = 1;
        let mut tokens = vec![array; MAX_DEPTH as usize + 1];
        tokens.push(token(DDOG_TRACER_VALUE_NIL));
        assert!(unsafe { encode(&tokens) }.is_err());

        let mut map = token(DDOG_TRACER_VALUE_MAP);
        map.child_count = u32::MAX;
        assert!(unsafe { encode(&[map]) }.is_err());
    }

    #[test]
    fn encodes_length_boundaries() {
        for (kind, marker32, marker255, marker256, marker65536) in [
            (DDOG_TRACER_VALUE_STRING, 0xd9, 0xd9, 0xda, 0xdb),
            (DDOG_TRACER_VALUE_BINARY, 0xc4, 0xc4, 0xc5, 0xc6),
        ] {
            for (len, expected) in [
                (32, vec![marker32, 32]),
                (255, vec![marker255, 255]),
                (256, vec![marker256, 1, 0]),
                (65_536, vec![marker65536, 0, 1, 0, 0]),
            ] {
                let bytes = vec![b'a'; len];
                let mut value = token(kind);
                value.bytes = ByteSlice::from(bytes.as_slice());
                let encoded = unsafe { encode(&[value]).unwrap() };
                assert_eq!(&encoded[..expected.len()], expected);
                assert_eq!(encoded.len(), expected.len() + len);
            }
        }

        let bytes31 = [b'a'; 31];
        let mut string31 = token(DDOG_TRACER_VALUE_STRING);
        string31.bytes = ByteSlice::from(&bytes31[..]);
        assert_eq!(unsafe { encode(&[string31]).unwrap() }[0], 0xbf);

        for (kind, count, header, values_per_entry) in [
            (DDOG_TRACER_VALUE_ARRAY, 15, vec![0x9f], 1),
            (DDOG_TRACER_VALUE_ARRAY, 16, vec![0xdc, 0, 16], 1),
            (DDOG_TRACER_VALUE_MAP, 15, vec![0x8f], 2),
            (DDOG_TRACER_VALUE_MAP, 16, vec![0xde, 0, 16], 2),
        ] {
            let mut container = token(kind);
            container.child_count = count;
            let mut tokens = vec![container];
            tokens.extend(std::iter::repeat_n(
                token(DDOG_TRACER_VALUE_NIL),
                count as usize * values_per_entry,
            ));
            let encoded = unsafe { encode(&tokens).unwrap() };
            assert_eq!(&encoded[..header.len()], header);
        }
    }

    #[test]
    fn rejects_invalid_token_slice() {
        let tokens = unsafe { Slice::from_raw_parts(std::ptr::null(), 1) };
        let mut handle = MaybeUninit::<Box<TracerEncodedValue>>::uninit();
        let out = NonNull::new(handle.as_mut_ptr()).unwrap();
        let error = unsafe { ddog_tracer_encode_value(tokens, out) };
        assert!(error.is_some());
    }

    #[test]
    fn null_blob_access_is_empty_and_free_is_safe() {
        assert!(ddog_tracer_encoded_value_as_slice(None).is_empty());
        ddog_tracer_encoded_value_free(None);
    }

    #[test]
    fn returned_blob_does_not_borrow_token_bytes() {
        let mut backing = b"stable".to_vec();
        let blob = {
            let mut string = token(DDOG_TRACER_VALUE_STRING);
            string.bytes = ByteSlice::from(backing.as_slice());
            unsafe { encode_blob(&[string]).unwrap() }
        };

        backing.fill(b'x');
        assert_eq!(
            ddog_tracer_encoded_value_as_slice(Some(&blob)).as_bytes(),
            b"\xa6stable"
        );
        ddog_tracer_encoded_value_free(Some(blob));
    }
}
