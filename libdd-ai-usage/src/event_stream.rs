// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! AWS event stream framing (`application/vnd.amazon.eventstream`), as
//! Bedrock's ConverseStream and InvokeModelWithResponseStream send it.
//!
//! A message is a 12-byte prelude (its total length and its headers' length,
//! big-endian `u32`s, and the CRC-32 of those 8 bytes), its headers, its
//! payload, and the CRC-32 of everything before it. A header is a one-byte
//! name length, the name, a one-byte value type and the value; only string
//! values (`:message-type`, `:event-type`, `:exception-type`, ...) are read.
//!
//! Messages are read from chunks that can end anywhere. A message longer
//! than the bound is skipped as it passes, without being held. A prelude
//! that fails its checksum leaves no way to find the next message, so the
//! rest of the stream is ignored.

/// The bytes of a prelude.
pub(crate) const PRELUDE_BYTES: usize = 12;
/// The bytes of the message checksum that ends a message.
const TRAILER_BYTES: usize = 4;
/// The value type of a string header.
const STRING: u8 = 7;
/// The value type of a byte-array header, sized like a string.
const BYTES: u8 = 6;

/// CRC-32 (IEEE 802.3, as zlib computes it), a byte at a time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc = CRC_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The total length a prelude states, or `None` when it fails its checksum
/// or states lengths no message can have.
fn prelude(bytes: &[u8]) -> Option<usize> {
    let total = be32(&bytes[0..4]) as usize;
    let headers = be32(&bytes[4..8]) as usize;
    if crc32(&bytes[0..8]) != be32(&bytes[8..12]) {
        return None;
    }
    let overhead = PRELUDE_BYTES + TRAILER_BYTES;
    (total >= overhead && headers <= total - overhead).then_some(total)
}

/// One whole message: the string headers this module reads, and its payload.
#[derive(Debug, Default)]
pub(crate) struct Message<'a> {
    /// `:message-type`: `event`, `exception` or `error`.
    pub message_type: Option<&'a str>,
    /// `:event-type` of an event (`messageStop`, `metadata`, `chunk`, ...).
    pub event_type: Option<&'a str>,
    /// `:exception-type` of an exception (`throttlingException`, ...).
    pub exception_type: Option<&'a str>,
    /// `:error-code` and `:error-message` of an error message.
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub payload: &'a [u8],
    /// The whole message, to check its checksum only when it is read.
    bytes: &'a [u8],
}

impl Message<'_> {
    /// Whether the message matches its checksum. Checked only for the
    /// messages a reader reads, not the content deltas around them.
    pub(crate) fn is_intact(&self) -> bool {
        let (body, trailer) = self.bytes.split_at(self.bytes.len() - TRAILER_BYTES);
        crc32(body) == be32(trailer)
    }
}

/// A whole message whose prelude was checked, or `None` when its headers do
/// not parse.
fn message(bytes: &[u8]) -> Option<Message<'_>> {
    let headers_len = be32(&bytes[4..8]) as usize;
    let mut headers = &bytes[PRELUDE_BYTES..PRELUDE_BYTES + headers_len];
    let mut message = Message {
        payload: &bytes[PRELUDE_BYTES + headers_len..bytes.len() - TRAILER_BYTES],
        bytes,
        ..Message::default()
    };
    while let Some((&name_len, rest)) = headers.split_first() {
        let name_len = usize::from(name_len);
        let (name, rest) = (rest.get(..name_len)?, rest.get(name_len..)?);
        let (&kind, rest) = rest.split_first()?;
        let (value, rest) = match kind {
            0 | 1 => (None, rest),
            2 => (None, rest.get(1..)?),
            3 => (None, rest.get(2..)?),
            4 => (None, rest.get(4..)?),
            5 | 8 => (None, rest.get(8..)?),
            9 => (None, rest.get(16..)?),
            BYTES | STRING => {
                let len = usize::from(u16::from_be_bytes([*rest.first()?, *rest.get(1)?]));
                let value = rest.get(2..2 + len)?;
                ((kind == STRING).then_some(value), rest.get(2 + len..)?)
            }
            _ => return None,
        };
        headers = rest;
        let value = value.and_then(|v| std::str::from_utf8(v).ok());
        match name {
            b":message-type" => message.message_type = value,
            b":event-type" => message.event_type = value,
            b":exception-type" => message.exception_type = value,
            b":error-code" => message.error_code = value,
            b":error-message" => message.error_message = value,
            _ => {}
        }
    }
    Some(message)
}

/// The messages of an event stream, read from chunks that can end anywhere.
#[derive(Debug, Clone, Default)]
pub(crate) struct Frames {
    /// The start of a message split across chunks.
    held: Vec<u8>,
    /// Bytes of a message over the bound still to pass.
    skip: usize,
    /// A prelude failed its checksum: nothing more can be read.
    broken: bool,
}

impl Frames {
    /// Read the next bytes of the stream, calling `on_message` for each
    /// message no longer than `max_message` bytes they complete.
    pub(crate) fn feed(
        &mut self,
        mut chunk: &[u8],
        max_message: usize,
        on_message: &mut dyn FnMut(&Message<'_>),
    ) {
        while !chunk.is_empty() && !self.broken {
            if self.skip > 0 {
                let n = self.skip.min(chunk.len());
                self.skip -= n;
                chunk = &chunk[n..];
                continue;
            }
            if self.held.is_empty() && chunk.len() >= PRELUDE_BYTES {
                // Read the messages a chunk holds whole where they are.
                match prelude(chunk) {
                    None => {
                        self.broken = true;
                        return;
                    }
                    Some(total) if total > max_message => {
                        self.skip = total;
                        continue;
                    }
                    Some(total) if chunk.len() >= total => {
                        if let Some(message) = message(&chunk[..total]) {
                            on_message(&message);
                        }
                        chunk = &chunk[total..];
                        continue;
                    }
                    Some(_) => {}
                }
            }
            if self.held.len() < PRELUDE_BYTES {
                let n = (PRELUDE_BYTES - self.held.len()).min(chunk.len());
                self.held.extend_from_slice(&chunk[..n]);
                chunk = &chunk[n..];
                if self.held.len() < PRELUDE_BYTES {
                    return;
                }
                match prelude(&self.held) {
                    None => {
                        self.broken = true;
                        self.held = Vec::new();
                        return;
                    }
                    Some(total) if total > max_message => {
                        self.skip = total - PRELUDE_BYTES;
                        self.held.clear();
                        continue;
                    }
                    Some(_) => {}
                }
            }
            let total = be32(&self.held[0..4]) as usize;
            let n = (total - self.held.len()).min(chunk.len());
            self.held.extend_from_slice(&chunk[..n]);
            chunk = &chunk[n..];
            if self.held.len() == total {
                if let Some(message) = message(&self.held) {
                    on_message(&message);
                }
                self.held.clear();
            }
        }
    }
}

/// One event stream message with string headers, for tests.
#[cfg(test)]
pub(crate) fn encode(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut encoded_headers = Vec::new();
    for (name, value) in headers {
        encoded_headers.push(name.len() as u8);
        encoded_headers.extend_from_slice(name.as_bytes());
        encoded_headers.push(STRING);
        encoded_headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
        encoded_headers.extend_from_slice(value.as_bytes());
    }
    let total = PRELUDE_BYTES + encoded_headers.len() + payload.len() + TRAILER_BYTES;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(encoded_headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32(&out);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&encoded_headers);
    out.extend_from_slice(payload);
    let message_crc = crc32(&out);
    out.extend_from_slice(&message_crc.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(stream: &[u8], step: usize, max: usize) -> Vec<(String, String, bool)> {
        let mut frames = Frames::default();
        let mut seen = Vec::new();
        for piece in stream.chunks(step) {
            frames.feed(piece, max, &mut |m| {
                seen.push((
                    m.event_type.or(m.exception_type).unwrap_or("").to_string(),
                    String::from_utf8_lossy(m.payload).to_string(),
                    m.is_intact(),
                ));
            });
        }
        seen
    }

    #[test]
    fn crc32_is_zlibs() {
        // The check value of CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn messages_split_anywhere_read_alike() {
        let mut stream = encode(
            &[(":event-type", "messageStart"), (":message-type", "event")],
            br#"{"role":"assistant"}"#,
        );
        stream.extend(encode(
            &[(":event-type", "metadata"), (":message-type", "event")],
            br#"{"usage":{"inputTokens":1}}"#,
        ));
        let whole = read(&stream, stream.len(), 1 << 20);
        assert_eq!(whole.len(), 2);
        assert_eq!(whole[1].0, "metadata");
        assert!(whole.iter().all(|(_, _, intact)| *intact));
        for step in [1, 2, 5, 11, 12, 13, 40] {
            assert_eq!(read(&stream, step, 1 << 20), whole, "{step}");
        }
    }

    #[test]
    fn long_messages_are_skipped_and_bad_preludes_stop_the_stream() {
        let long = encode(&[(":event-type", "chunk")], &[b'x'; 300]);
        let short = encode(&[(":event-type", "metadata")], b"{}");
        let stream = [long.clone(), short.clone()].concat();
        for step in [1, 7, stream.len()] {
            let seen = read(&stream, step, 200);
            assert_eq!(seen.len(), 1, "{step}");
            assert_eq!(seen[0].0, "metadata");
        }

        let mut corrupt = short.clone();
        corrupt[9] ^= 1;
        let stream = [corrupt, short.clone()].concat();
        assert!(read(&stream, stream.len(), 1 << 20).is_empty());

        // A payload that fails its checksum is still framed: the next
        // message is read.
        let mut damaged = short.clone();
        let at = damaged.len() - 5;
        damaged[at] ^= 1;
        let stream = [damaged, short].concat();
        let seen = read(&stream, 3, 1 << 20);
        assert_eq!(
            seen.iter()
                .map(|(_, _, intact)| *intact)
                .collect::<Vec<_>>(),
            [false, true]
        );
    }
}
