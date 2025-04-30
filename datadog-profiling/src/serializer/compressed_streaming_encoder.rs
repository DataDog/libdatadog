// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use bytes::BufMut;
use lz4_flex::frame::FrameEncoder;
use prost::encoding::{encode_key, encode_varint, encoded_len_varint, key_len, WireType};
use std::io::Write;

pub struct CompressedProtobufSerializer {
    buffer: Vec<u8>,
    zipper: FrameEncoder<Vec<u8>>,
}

// I've opened a PR for a generic version of this upstream:
// https://github.com/tokio-rs/prost/pull/978
fn encode_str(tag: u32, value: &str, buf: &mut Vec<u8>) {
    encode_key(tag, WireType::LengthDelimited, buf);
    encode_varint(value.len() as u64, buf);
    buf.put_slice(value.as_bytes());
}

impl CompressedProtobufSerializer {
    pub fn encode(&mut self, item: impl prost::Message) -> anyhow::Result<()> {
        item.encode(&mut self.buffer)?;
        self.zipper.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }

    /// Only meant for string table strings. This is essentially an
    /// implementation of [prost::Message::encode] but for any `AsRef<str>`,
    /// and specialized for handling the unlikely OOM conditions of writing
    /// into a `Vec<u8>`.
    pub(crate) fn encode_string_table_entry(
        &mut self,
        item: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        // In pprof, string tables are tag 6 on the Profile message.
        let tag = 6u32;
        let str = item.as_ref();
        let encoded_len = encoded_len_varint(str.len() as u64);
        let required = key_len(tag) + encoded_len + str.len();
        if let Err(err) = self.buffer.try_reserve(required) {
            return Err(anyhow::Error::from(err)
                .context("failed to encode Protobuf str; insufficient buffer capacity"));
        }

        encode_str(tag, str, &mut self.buffer);
        self.zipper.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }

    pub fn finish(self) -> anyhow::Result<Vec<u8>> {
        Ok(self.zipper.finish()?)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let buffer = Vec::with_capacity(capacity);
        let zipper = FrameEncoder::new(Vec::with_capacity(capacity));
        Self { buffer, zipper }
    }
}
