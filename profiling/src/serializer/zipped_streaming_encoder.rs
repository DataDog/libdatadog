use std::io::Write;

use lz4_flex::frame::FrameEncoder;

pub struct ZippedProtobufSerializer {
    buffer: Vec<u8>,
    zipper: FrameEncoder<Vec<u8>>,
}

impl ZippedProtobufSerializer {
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
