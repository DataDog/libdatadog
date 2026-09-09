// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{ser::Error as _, Serialize, Serializer};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

pub(crate) struct FixedHex<const N: usize>([u8; N]);

impl<const N: usize> Serialize for FixedHex<N> {
    #[inline]
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = std::str::from_utf8(&self.0).map_err(S::Error::custom)?;
        serializer.serialize_str(value)
    }
}

#[inline]
fn fixed_hex<const N: usize>(mut value: u128) -> FixedHex<N> {
    let mut encoded = [b'0'; N];
    let mut index = N;

    while index != 0 {
        index -= 1;
        // The mask guarantees that the conversion fits and indexes HEX_DIGITS.
        encoded[index] = HEX_DIGITS[(value & 0x0f) as usize];
        value >>= 4;
    }

    FixedHex(encoded)
}

#[inline]
pub(crate) fn hex_u64(value: u64) -> FixedHex<16> {
    fixed_hex(u128::from(value))
}

#[inline]
pub(crate) fn hex_low_u64(value: u128) -> FixedHex<16> {
    fixed_hex(value)
}

#[inline]
pub(crate) fn hex_u128(value: u128) -> FixedHex<32> {
    fixed_hex(value)
}
