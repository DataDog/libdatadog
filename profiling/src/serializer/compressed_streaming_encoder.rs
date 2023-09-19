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

    pub fn finish(self) -> anyhow::Result<Vec<u8>> {
        Ok(self.zipper.finish()?)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let buffer = Vec::with_capacity(capacity);
        let zipper = FrameEncoder::new(Vec::with_capacity(capacity));
        Self { buffer, zipper }
    }
}
