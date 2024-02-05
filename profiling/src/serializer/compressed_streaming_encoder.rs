// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use lz4_flex::frame::FrameEncoder;
use std::io::Write;

pub struct CompressedProtobufSerializer {
    buffer: Vec<u8>,
    zipper: FrameEncoder<Vec<u8>>,
}

impl CompressedProtobufSerializer {
    pub fn encode(&mut self, item: impl prost::Message) -> anyhow::Result<()> {
        item.encode(&mut self.buffer)?;
        self.zipper.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }

    /// Only meant for string table strings.
    pub(crate) fn encode_str(&mut self, item: impl AsRef<str>) -> anyhow::Result<()> {
        let tag = 6u32;
        let str = item.as_ref();
        let encoded_len = prost::encoding::encoded_len_varint(str.len() as u64);
        let required = prost::encoding::key_len(tag) + encoded_len + str.len();
        if let Err(err) = self.buffer.try_reserve(required) {
            return Err(anyhow::Error::from(err)
                .context("failed to encode Protobuf message; insufficient buffer capacity"));
        }

        prost::encoding::string::encode_ref(tag, item, &mut self.buffer);
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
