// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "compression")]
use std::io::Write as _;

#[cfg(feature = "compression")]
const CONTENT_ENCODING_ZSTD: http::HeaderValue = http::HeaderValue::from_static("zstd");
#[cfg(feature = "compression")]
const DEFAULT_COMPRESSION_LEVEL: i32 = 3;

#[cfg(all(feature = "compression", not(target_arch = "wasm32")))]
type ZstdEncoder = zstd::Encoder<'static, Vec<u8>>;
#[cfg(all(feature = "compression", target_arch = "wasm32"))]
type ZstdEncoder = zrip::FrameEncoder<Vec<u8>>;

#[cfg(feature = "compression")]
fn zstd_compression_level(level: i32) -> i32 {
    if level == 0 {
        DEFAULT_COMPRESSION_LEVEL
    } else {
        level
    }
}

#[cfg(all(feature = "compression", not(target_arch = "wasm32")))]
fn new_zstd_encoder(writer: Vec<u8>, level: i32) -> std::io::Result<ZstdEncoder> {
    zstd::Encoder::new(writer, zstd_compression_level(level))
}

#[cfg(all(feature = "compression", target_arch = "wasm32"))]
fn new_zstd_encoder(writer: Vec<u8>, level: i32) -> std::io::Result<ZstdEncoder> {
    zrip::FrameEncoder::new(writer, zstd_compression_level(level)).map_err(std::io::Error::other)
}

#[derive(Clone, Copy, Debug)]
pub enum CompressionStrategy {
    None,
    #[cfg(feature = "compression")]
    /// Zstd-compatible compression.
    ///
    /// WASM accepts levels `-7..=4`. Native targets accept the range reported by
    /// `zstd::compression_level_range()`, making `-7..=4` the portable range.
    /// Level `0` selects level `3` on every target.
    Zstd {
        level: i32,
    },
}

/// Returns the compressed data, and the actual compression strategy used.
/// If an error happens during compression, defaults to [`CompressionStrategy::None`]
pub fn compress(data: Vec<u8>, strategy: CompressionStrategy) -> (Vec<u8>, CompressionStrategy) {
    match strategy {
        CompressionStrategy::None => (data, CompressionStrategy::None),
        #[cfg(feature = "compression")]
        CompressionStrategy::Zstd { level } => {
            // Start with an initial buffer
            // Allocate 1/10th of the original buffer, so we shouldn't add too
            // much memory usage, and no less than 256 bytes
            let writer = Vec::with_capacity((data.len() / 10).max(256));
            let result = new_zstd_encoder(writer, level).and_then(|mut encoder| {
                encoder.write_all(&data)?;
                Ok((encoder.finish()?, strategy))
            });
            result.unwrap_or((data, CompressionStrategy::None))
        }
    }
}

pub fn add_headers(headers: &mut http::HeaderMap, strategy: CompressionStrategy) {
    match strategy {
        CompressionStrategy::None => {
            let _ = headers;
        }
        #[cfg(feature = "compression")]
        CompressionStrategy::Zstd { .. } => {
            headers.insert(http::header::CONTENT_ENCODING, CONTENT_ENCODING_ZSTD);
        }
    }
}

#[cfg(all(test, feature = "compression", not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn decompress(data: &[u8]) -> std::io::Result<Vec<u8>> {
        zstd::decode_all(data)
    }

    #[test]
    fn zstd_compression_roundtrips() {
        let data = b"hello zstd".repeat(100);
        let (compressed, strategy) = compress(data.clone(), CompressionStrategy::Zstd { level: 1 });

        assert!(matches!(strategy, CompressionStrategy::Zstd { level: 1 }));
        assert_eq!(decompress(&compressed).unwrap(), data);
    }

    #[test]
    fn zero_uses_default_compression_level() {
        let data = b"hello zstd".repeat(100);
        let (default_compressed, _) =
            compress(data.clone(), CompressionStrategy::Zstd { level: 0 });
        let (level_three_compressed, _) = compress(
            data,
            CompressionStrategy::Zstd {
                level: DEFAULT_COMPRESSION_LEVEL,
            },
        );

        assert_eq!(default_compressed, level_three_compressed);
    }
}
