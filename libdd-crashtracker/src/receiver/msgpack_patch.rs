// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Minimal msgpack patcher for appending entries to the `meta` map inside a
//! pre-encoded v0.4 trace span.
//!
//! The v0.4 payload is `[[{span}]]`. The span is a msgpack map containing a
//! `meta` key whose value is also a map. We locate the meta map header, bump
//! its entry count, and splice in new key-value pairs at the end of the meta
//! map — without decoding or re-encoding the rest of the payload.
//!
//! This works because:
//! - msgpack map counts are fixed-width (fixmap 4 bits, map16 2 bytes, map32 4 bytes) — no varint,
//!   so incrementing doesn't change the byte length.
//! - map entries are read sequentially: appending after the last existing entry (but before the
//!   next span-level field) is valid as long as the count matches.

/// Append key-value string pairs to the `meta` map of the first span in a
/// `[[{span}]]` v0.4 trace payload. Returns a new payload with the patched
/// map count and spliced entries, or `None` if the payload can't be patched.
pub(crate) fn patch_span_meta(payload: &[u8], entries: &[(&str, &str)]) -> Option<Vec<u8>> {
    if entries.is_empty() {
        return Some(payload.to_vec());
    }

    let mut pos = 0;

    // Skip outer array(1) and inner array(1)
    skip_array_header(payload, &mut pos)?;
    skip_array_header(payload, &mut pos)?;

    // Now at the span map header
    let span_map_count = read_map_count(payload, &mut pos)?;

    // Walk span map fields to find "meta"
    for _ in 0..span_map_count {
        let key_start = pos;
        let key = read_str(payload, &mut pos)?;
        if key == "meta" {
            // Found it. pos is now at the meta map value header.
            let meta_header_pos = pos;
            let meta_count = read_map_count(payload, &mut pos)?;
            let new_count = meta_count + entries.len() as u32;

            // Skip existing meta entries to find the splice point
            for _ in 0..meta_count {
                skip_value(payload, &mut pos)?; // key
                skip_value(payload, &mut pos)?; // value
            }
            let splice_pos = pos; // insert new entries here

            // Build the patched payload
            let mut out = Vec::with_capacity(payload.len() + entries.len() * 64);

            // Everything before the meta map header
            out.extend_from_slice(&payload[..meta_header_pos]);

            // Patched meta map header with new count
            write_map_header(&mut out, new_count);

            // Existing meta entries (skip the original header bytes)
            let original_header_len =
                meta_header_pos + map_header_len(payload, meta_header_pos)? - meta_header_pos;
            out.extend_from_slice(&payload[meta_header_pos + original_header_len..splice_pos]);

            // New entries
            for (k, v) in entries {
                write_str(&mut out, k);
                write_str(&mut out, v);
            }

            // Everything after the meta map
            out.extend_from_slice(&payload[splice_pos..]);

            return Some(out);
        } else {
            // Not "meta" — skip its value
            let _ = key_start; // suppress unused warning
            skip_value(payload, &mut pos)?;
        }
    }

    // No "meta" field found — can't patch
    None
}

// ── msgpack readers (just enough to navigate, not decode) ──────────────

fn skip_array_header(data: &[u8], pos: &mut usize) -> Option<u32> {
    let b = *data.get(*pos)?;
    if b & 0xf0 == 0x90 {
        *pos += 1;
        Some(u32::from(b & 0x0f))
    } else if b == 0xdc {
        let count = u16::from_be_bytes([*data.get(*pos + 1)?, *data.get(*pos + 2)?]);
        *pos += 3;
        Some(u32::from(count))
    } else if b == 0xdd {
        let count = u32::from_be_bytes([
            *data.get(*pos + 1)?,
            *data.get(*pos + 2)?,
            *data.get(*pos + 3)?,
            *data.get(*pos + 4)?,
        ]);
        *pos += 5;
        Some(count)
    } else {
        None
    }
}

fn read_map_count(data: &[u8], pos: &mut usize) -> Option<u32> {
    let b = *data.get(*pos)?;
    if b & 0xf0 == 0x80 {
        *pos += 1;
        Some(u32::from(b & 0x0f))
    } else if b == 0xde {
        let count = u16::from_be_bytes([*data.get(*pos + 1)?, *data.get(*pos + 2)?]);
        *pos += 3;
        Some(u32::from(count))
    } else if b == 0xdf {
        let count = u32::from_be_bytes([
            *data.get(*pos + 1)?,
            *data.get(*pos + 2)?,
            *data.get(*pos + 3)?,
            *data.get(*pos + 4)?,
        ]);
        *pos += 5;
        Some(count)
    } else {
        None
    }
}

fn map_header_len(data: &[u8], pos: usize) -> Option<usize> {
    let b = *data.get(pos)?;
    if b & 0xf0 == 0x80 {
        Some(1)
    } else if b == 0xde {
        Some(3)
    } else if b == 0xdf {
        Some(5)
    } else {
        None
    }
}

fn read_str<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a str> {
    let len = read_str_len(data, pos)?;
    let s = core::str::from_utf8(data.get(*pos..*pos + len)?).ok()?;
    *pos += len;
    Some(s)
}

fn read_str_len(data: &[u8], pos: &mut usize) -> Option<usize> {
    let b = *data.get(*pos)?;
    if b & 0xe0 == 0xa0 {
        *pos += 1;
        Some(usize::from(b & 0x1f))
    } else if b == 0xd9 {
        let len = usize::from(*data.get(*pos + 1)?);
        *pos += 2;
        Some(len)
    } else if b == 0xda {
        let len = usize::from(u16::from_be_bytes([
            *data.get(*pos + 1)?,
            *data.get(*pos + 2)?,
        ]));
        *pos += 3;
        Some(len)
    } else if b == 0xdb {
        let len = u32::from_be_bytes([
            *data.get(*pos + 1)?,
            *data.get(*pos + 2)?,
            *data.get(*pos + 3)?,
            *data.get(*pos + 4)?,
        ]) as usize;
        *pos += 5;
        Some(len)
    } else {
        None
    }
}

/// Skip a single msgpack value (any type) by advancing `pos` past it.
fn skip_value(data: &[u8], pos: &mut usize) -> Option<()> {
    let b = *data.get(*pos)?;
    match b {
        // positive fixint
        0x00..=0x7f => {
            *pos += 1;
        }
        // negative fixint
        0xe0..=0xff => {
            *pos += 1;
        }
        // fixmap
        b if b & 0xf0 == 0x80 => {
            let count = u32::from(b & 0x0f);
            *pos += 1;
            for _ in 0..count * 2 {
                skip_value(data, pos)?;
            }
        }
        // fixarray
        b if b & 0xf0 == 0x90 => {
            let count = u32::from(b & 0x0f);
            *pos += 1;
            for _ in 0..count {
                skip_value(data, pos)?;
            }
        }
        // fixstr
        b if b & 0xe0 == 0xa0 => {
            let len = usize::from(b & 0x1f);
            *pos += 1 + len;
        }
        0xc0 => *pos += 1, // nil
        0xc2 => *pos += 1, // false
        0xc3 => *pos += 1, // true
        0xc4 => {
            let len = usize::from(*data.get(*pos + 1)?);
            *pos += 2 + len;
        } // bin8
        0xc5 => {
            let len = usize::from(u16::from_be_bytes([
                *data.get(*pos + 1)?,
                *data.get(*pos + 2)?,
            ]));
            *pos += 3 + len;
        } // bin16
        0xc6 => {
            let len = u32::from_be_bytes([
                *data.get(*pos + 1)?,
                *data.get(*pos + 2)?,
                *data.get(*pos + 3)?,
                *data.get(*pos + 4)?,
            ]) as usize;
            *pos += 5 + len;
        } // bin32
        0xca => *pos += 5, // float32
        0xcb => *pos += 9, // float64
        0xcc => *pos += 2, // uint8
        0xcd => *pos += 3, // uint16
        0xce => *pos += 5, // uint32
        0xcf => *pos += 9, // uint64
        0xd0 => *pos += 2, // int8
        0xd1 => *pos += 3, // int16
        0xd2 => *pos += 5, // int32
        0xd3 => *pos += 9, // int64
        0xd9 => {
            let len = usize::from(*data.get(*pos + 1)?);
            *pos += 2 + len;
        } // str8
        0xda => {
            let len = usize::from(u16::from_be_bytes([
                *data.get(*pos + 1)?,
                *data.get(*pos + 2)?,
            ]));
            *pos += 3 + len;
        } // str16
        0xdb => {
            let len = u32::from_be_bytes([
                *data.get(*pos + 1)?,
                *data.get(*pos + 2)?,
                *data.get(*pos + 3)?,
                *data.get(*pos + 4)?,
            ]) as usize;
            *pos += 5 + len;
        } // str32
        0xdc => {
            let count = u16::from_be_bytes([*data.get(*pos + 1)?, *data.get(*pos + 2)?]) as u32;
            *pos += 3;
            for _ in 0..count {
                skip_value(data, pos)?;
            }
        } // array16
        0xdd => {
            let count = u32::from_be_bytes([
                *data.get(*pos + 1)?,
                *data.get(*pos + 2)?,
                *data.get(*pos + 3)?,
                *data.get(*pos + 4)?,
            ]);
            *pos += 5;
            for _ in 0..count {
                skip_value(data, pos)?;
            }
        } // array32
        0xde => {
            let count = u16::from_be_bytes([*data.get(*pos + 1)?, *data.get(*pos + 2)?]) as u32;
            *pos += 3;
            for _ in 0..count * 2 {
                skip_value(data, pos)?;
            }
        } // map16
        0xdf => {
            let count = u32::from_be_bytes([
                *data.get(*pos + 1)?,
                *data.get(*pos + 2)?,
                *data.get(*pos + 3)?,
                *data.get(*pos + 4)?,
            ]);
            *pos += 5;
            for _ in 0..count * 2 {
                skip_value(data, pos)?;
            }
        } // map32
        _ => return None,  // ext types etc. — not expected in trace payloads
    }
    if *pos > data.len() {
        return None;
    }
    Some(())
}

// ── msgpack writers ────────────────────────────────────────────────────

fn write_map_header(out: &mut Vec<u8>, count: u32) {
    if count <= 15 {
        out.push(0x80 | (count as u8));
    } else if count <= u32::from(u16::MAX) {
        out.push(0xde);
        out.extend_from_slice(&(count as u16).to_be_bytes());
    } else {
        out.push(0xdf);
        out.extend_from_slice(&count.to_be_bytes());
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let len = s.len();
    if len < 32 {
        out.push(0xa0 | (len as u8));
    } else if len < 256 {
        out.push(0xd9);
        out.push(len as u8);
    } else if len <= usize::from(u16::MAX) {
        out.push(0xda);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0xdb);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(s.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `[[{span}]]` with a meta map.
    fn make_payload(meta_entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(0x91); // array(1)
        buf.push(0x91); // array(1)

        // span map with 3 fields: name, service, meta
        write_map_header(&mut buf, 3);
        write_str(&mut buf, "name");
        write_str(&mut buf, "test.span");
        write_str(&mut buf, "service");
        write_str(&mut buf, "test-svc");
        write_str(&mut buf, "meta");
        write_map_header(&mut buf, meta_entries.len() as u32);
        for (k, v) in meta_entries {
            write_str(&mut buf, k);
            write_str(&mut buf, v);
        }
        buf
    }

    #[test]
    fn patch_appends_to_meta() {
        let payload = make_payload(&[("existing", "value")]);
        let patched = patch_span_meta(&payload, &[("error.message", "crash"), ("uuid", "abc-123")])
            .expect("patch should succeed");

        // Decode and verify with rmpv
        let val = rmpv::decode::read_value(&mut &patched[..]).expect("decode");
        let traces = val.as_array().unwrap();
        let spans = traces[0].as_array().unwrap();
        let span = spans[0].as_map().unwrap();
        let meta = span
            .iter()
            .find(|(k, _)| k.as_str() == Some("meta"))
            .unwrap()
            .1
            .as_map()
            .unwrap();

        assert_eq!(meta.len(), 3); // existing + 2 new
        let find = |key: &str| {
            meta.iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v.as_str().unwrap())
        };
        assert_eq!(find("existing"), Some("value"));
        assert_eq!(find("error.message"), Some("crash"));
        assert_eq!(find("uuid"), Some("abc-123"));
    }

    #[test]
    fn patch_empty_meta() {
        let payload = make_payload(&[]);
        let patched = patch_span_meta(&payload, &[("key", "val")]).expect("patch");
        let val = rmpv::decode::read_value(&mut &patched[..]).expect("decode");
        let meta = val.as_array().unwrap()[0].as_array().unwrap()[0]
            .as_map()
            .unwrap()
            .iter()
            .find(|(k, _)| k.as_str() == Some("meta"))
            .unwrap()
            .1
            .as_map()
            .unwrap();
        assert_eq!(meta.len(), 1);
    }

    #[test]
    fn patch_no_entries_is_noop() {
        let payload = make_payload(&[("a", "b")]);
        let patched = patch_span_meta(&payload, &[]).unwrap();
        assert_eq!(patched, payload);
    }
}
