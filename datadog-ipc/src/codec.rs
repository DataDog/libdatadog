// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Codec for IPC messages.
//!
//! Request wire format: `[4 bytes: u32 LE discriminant][N bytes: bincode payload]`
//! Response wire format: `[N bytes: bincode payload]` (no discriminant)
//! Ack wire format: `[0 bytes]` (empty datagram)

use serde::{de::DeserializeOwned, Serialize};
use std::fmt;

pub const DISCRIMINANT_SIZE: usize = 4;

/// Encode a request: 4-byte LE discriminant prefix + bincode payload.
pub fn encode<T: Serialize>(discriminant: u32, value: &T) -> Vec<u8> {
    let payload = bincode::serialize(value).unwrap_or_default();
    let mut buf = Vec::with_capacity(DISCRIMINANT_SIZE + payload.len());
    buf.extend_from_slice(&discriminant.to_le_bytes());
    buf.extend_from_slice(&payload);
    buf
}

/// Decode a request: returns `(discriminant, value)`.
pub fn decode<T: DeserializeOwned>(buf: &[u8]) -> Result<(u32, T), DecodeError> {
    if buf.len() < DISCRIMINANT_SIZE {
        return Err(DecodeError::TooShort);
    }
    let disc_bytes: [u8; 4] = buf[..DISCRIMINANT_SIZE].try_into().unwrap_or([0u8; 4]);
    let discriminant = u32::from_le_bytes(disc_bytes);
    let value = bincode::deserialize(&buf[DISCRIMINANT_SIZE..]).map_err(DecodeError::Bincode)?;
    Ok((discriminant, value))
}

/// Encode a response (no discriminant prefix).
pub fn encode_response<T: Serialize>(value: &T) -> Vec<u8> {
    bincode::serialize(value).unwrap_or_default()
}

/// Decode a response (no discriminant prefix).
pub fn decode_response<T: DeserializeOwned>(buf: &[u8]) -> Result<T, DecodeError> {
    bincode::deserialize(buf).map_err(DecodeError::Bincode)
}

#[derive(Debug)]
pub enum DecodeError {
    TooShort,
    Bincode(bincode::Error),
    Io(std::io::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooShort => write!(f, "IPC message too short (missing discriminant)"),
            DecodeError::Bincode(e) => write!(f, "IPC bincode decode error: {e}"),
            DecodeError::Io(e) => write!(f, "IPC I/O error: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}
