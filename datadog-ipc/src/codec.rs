// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Codec for IPC messages.
//!
//! Request wire format: `[N bytes: bincode payload]`
//! Response wire format: `[N bytes: bincode payload]` (no discriminant)
//! Ack wire format: `[1 byte: 0x00]`

use serde::{de::DeserializeOwned, Serialize};
use std::fmt;

/// Encode data as a bincode payload.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    #[allow(clippy::expect_used)]
    bincode::serialize(value).expect("Encoding the response failed. This should never happen")
}

/// Decode data from a bincode payload.
pub fn decode<T: DeserializeOwned>(buf: &[u8]) -> Result<T, DecodeError> {
    bincode::deserialize(buf).map_err(DecodeError::Bincode)
}

#[derive(Debug)]
pub enum DecodeError {
    Bincode(bincode::Error),
    Io(std::io::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Bincode(e) => write!(f, "IPC bincode decode error: {e}"),
            DecodeError::Io(e) => write!(f, "IPC I/O error: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}
