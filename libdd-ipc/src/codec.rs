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

/// Encode data as a bincode payload, reserving additional capacity after it.
pub fn encode_with_reserve<T: Serialize>(value: &T, additional_capacity: usize) -> Vec<u8> {
    #[allow(clippy::expect_used)]
    let encoded_len = usize::try_from(
        bincode::serialized_size(value)
            .expect("Calculating the encoded message size failed. This should never happen"),
    )
    .expect("The encoded message size does not fit in memory");
    #[allow(clippy::expect_used)]
    let capacity = encoded_len
        .checked_add(additional_capacity)
        .expect("The encoded message capacity overflowed");
    let mut encoded = Vec::with_capacity(capacity);
    #[allow(clippy::expect_used)]
    bincode::serialize_into(&mut encoded, value)
        .expect("Encoding the response failed. This should never happen");
    encoded
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

#[cfg(test)]
mod tests {
    use super::{encode, encode_with_reserve};

    #[test]
    fn encode_with_reserve_preserves_encoding_and_headroom() {
        let value = ("request", vec![1_u64, 2, 3]);
        let encoded = encode(&value);
        let encoded_with_reserve = encode_with_reserve(&value, 164);

        assert_eq!(encoded_with_reserve, encoded);
        assert!(encoded_with_reserve.capacity() - encoded_with_reserve.len() >= 164);
    }
}
