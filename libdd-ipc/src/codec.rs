// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Codec for IPC messages.
//!
//! Request wire format: `[N bytes: bincode payload]`
//! Response wire format: `[N bytes: bincode payload]` (no discriminant)
//! Ack wire format: `[1 byte: 0x00]`

use serde::{Serialize, de::DeserializeOwned};
use std::fmt;

/// Encode data as a bincode payload.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    #[allow(clippy::expect_used)]
    bincode::serialize(value).expect("Encoding the response failed. This should never happen")
}

/// Encode data as a bincode payload, reserving additional capacity after it.
pub fn encode_with_reserve<T: Serialize>(value: &T, additional_capacity: usize) -> Vec<u8> {
    let mut encoded = Vec::new();
    #[allow(clippy::expect_used)]
    bincode::serialize_into(&mut encoded, value)
        .expect("Encoding the response failed. This should never happen");
    encoded.reserve(additional_capacity);
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
    fn encode_with_reserve_serializes_once() {
        struct Counted(std::cell::Cell<usize>);
        impl serde::Serialize for Counted {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.set(self.0.get() + 1);
                serializer.serialize_u8(7)
            }
        }
        let value = Counted(std::cell::Cell::new(0));
        assert_eq!(encode_with_reserve(&value, 164), vec![7]);
        assert_eq!(value.0.get(), 1);
    }

    #[test]
    fn encode_with_reserve_preserves_encoding_and_headroom() {
        let value = ("request", vec![1_u64, 2, 3]);
        let encoded = encode(&value);
        let encoded_with_reserve = encode_with_reserve(&value, 164);

        assert_eq!(encoded_with_reserve, encoded);
        assert!(encoded_with_reserve.capacity() - encoded_with_reserve.len() >= 164);
    }
}
