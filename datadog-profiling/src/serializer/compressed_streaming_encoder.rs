// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::UploadCompression;
use bytes::BufMut;
use lz4_flex::frame::FrameEncoder as Lz4FrameEncoder;
use prost::encoding::{encode_key, encode_varint, encoded_len_varint, key_len, WireType};
use std::io::{self, Write};
use zstd::stream::Encoder as ZstdEncoder;

// None is not really for prod, so the fact it takes 0 space, creating a large
// discrepancy in size between the enum variants, is irrelevant.
#[allow(clippy::large_enum_variant)]
enum Compressor {
    None,
    Lz4 {
        encoder: Lz4FrameEncoder<Vec<u8>>,
    },
    Zstd {
        encoder: ZstdEncoder<'static, Vec<u8>>,
    },
}

impl Compressor {
    #[inline]
    fn compress(&mut self, buffer: &mut Vec<u8>) -> io::Result<()> {
        let writer: &mut dyn Write = match self {
            Compressor::None => return Ok(()),
            Compressor::Lz4 { encoder: zipper } => zipper,
            Compressor::Zstd { encoder } => encoder,
        };

        writer.write_all(buffer)?;
        buffer.clear();
        Ok(())
    }
}

pub struct CompressedProtobufSerializer {
    /// Buffer that protobuf is encoded into. Lz4 uses this as a temporary
    /// buffer, while None uses this as the final output buffer.
    buffer: Vec<u8>,
    compressor: Compressor,
}

// I've opened a PR for a generic version of this upstream:
// https://github.com/tokio-rs/prost/pull/978
fn encode_str(tag: u32, value: &str, buf: &mut Vec<u8>) {
    encode_key(tag, WireType::LengthDelimited, buf);
    encode_varint(value.len() as u64, buf);
    buf.put_slice(value.as_bytes());
}

impl CompressedProtobufSerializer {
    pub fn encode(&mut self, item: impl prost::Message) -> io::Result<()> {
        let buffer = &mut self.buffer;
        item.encode(buffer)?;
        self.compressor.compress(buffer)
    }

    /// Only meant for string table strings. This is essentially an
    /// implementation of [prost::Message::encode] but for any `AsRef<str>`,
    /// and specialized for handling the unlikely OOM conditions of writing
    /// into a `Vec<u8>`.
    pub(crate) fn encode_string_table_entry(&mut self, item: impl AsRef<str>) -> io::Result<()> {
        let buffer = &mut self.buffer;
        // In pprof, string tables are tag 6 on the Profile message.
        let tag = 6u32;
        let str = item.as_ref();
        let encoded_len = encoded_len_varint(str.len() as u64);
        let required = key_len(tag) + encoded_len + str.len();
        buffer.try_reserve(required)?;

        encode_str(tag, str, buffer);
        self.compressor.compress(buffer)
    }

    pub fn finish(self) -> io::Result<Vec<u8>> {
        match self.compressor {
            Compressor::None => Ok(self.buffer),
            Compressor::Lz4 { encoder: zipper } => {
                debug_assert!(self.buffer.is_empty());
                Ok(zipper.finish()?)
            }
            Compressor::Zstd { encoder } => encoder.finish(),
        }
    }

    pub fn with_config_and_capacity(
        config: UploadCompression,
        capacity: usize,
    ) -> io::Result<Self> {
        const TEMPORARY_BUFFER_SIZE: usize = 256;
        // Final output buffer.
        let buffer = Vec::with_capacity(capacity);
        Ok(match config {
            UploadCompression::Off => Self {
                buffer,
                compressor: Compressor::None,
            },
            UploadCompression::On | UploadCompression::Lz4 => Self {
                // Temporary input buffer.
                buffer: Vec::with_capacity(TEMPORARY_BUFFER_SIZE),
                compressor: Compressor::Lz4 {
                    encoder: Lz4FrameEncoder::new(buffer),
                },
            },
            UploadCompression::Zstd => Self {
                buffer: Vec::with_capacity(TEMPORARY_BUFFER_SIZE),
                compressor: Compressor::Zstd {
                    // A level of 0 uses zstd's default (currently 3).
                    encoder: ZstdEncoder::new(buffer, 0)?,
                },
            },
        })
    }
}
