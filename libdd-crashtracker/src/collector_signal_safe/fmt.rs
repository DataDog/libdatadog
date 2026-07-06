// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use heapless::String as HeaplessString;

use super::report::FRAME_IP_CAPACITY;

pub const HEX_U32_CAPACITY: usize = 10;
pub const I32_BUF_CAPACITY: usize = 12;

pub fn hex_addr(value: usize) -> HeaplessString<FRAME_IP_CAPACITY> {
    hex(value as u64, core::mem::size_of::<usize>() * 2)
}

pub fn hex_u32(value: u32) -> HeaplessString<HEX_U32_CAPACITY> {
    hex(value as u64, 8)
}

fn hex<const N: usize>(value: u64, digits: usize) -> HeaplessString<N> {
    let mut out = HeaplessString::new();
    let _ = out.push_str("0x");

    for shift in (0..digits).rev() {
        let nibble = ((value >> (shift * 4)) & 0xf) as u8;
        let ch = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        let _ = out.push(ch as char);
    }

    out
}

pub fn write_i32(value: i32, out: &mut [u8; I32_BUF_CAPACITY]) -> usize {
    let mut n = value as i64;
    let negative = n < 0;
    if negative {
        n = n.wrapping_neg();
    }

    let mut tmp = [0u8; 11];
    let mut len = 0usize;
    loop {
        tmp[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }

    let mut off = 0usize;
    if negative {
        out[0] = b'-';
        off = 1;
    }
    let mut i = 0usize;
    while i < len {
        out[off + i] = tmp[len - i - 1];
        i += 1;
    }
    off + len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_addr_covers_boundaries() {
        let zero = hex_addr(0);
        assert_eq!(zero.len(), FRAME_IP_CAPACITY);
        assert!(zero
            .as_str()
            .strip_prefix("0x")
            .unwrap()
            .bytes()
            .all(|b| b == b'0'));

        let max = hex_addr(usize::MAX);
        assert_eq!(max.len(), FRAME_IP_CAPACITY);
        assert!(max
            .as_str()
            .strip_prefix("0x")
            .unwrap()
            .bytes()
            .all(|b| b == b'f'));
    }

    #[test]
    fn hex_u32_covers_boundaries() {
        assert_eq!(hex_u32(0).as_str(), "0x00000000");
        assert_eq!(hex_u32(u32::MAX).as_str(), "0xffffffff");
        assert_eq!(hex_u32(0x1234abcd).as_str(), "0x1234abcd");
    }

    #[test]
    fn integer_debug_writer_handles_sign() {
        let mut buf = [0u8; I32_BUF_CAPACITY];
        let n = write_i32(-123, &mut buf);
        assert_eq!(&buf[..n], b"-123");
        let n = write_i32(42, &mut buf);
        assert_eq!(&buf[..n], b"42");
        let n = write_i32(0, &mut buf);
        assert_eq!(&buf[..n], b"0");
        let n = write_i32(i32::MIN, &mut buf);
        assert_eq!(&buf[..n], b"-2147483648");
    }
}
