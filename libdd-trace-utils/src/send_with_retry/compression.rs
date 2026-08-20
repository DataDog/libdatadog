// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "compression")]
use std::io::Write as _;

#[cfg(feature = "compression")]
const CONTENT_ENCODING_ZSTD: http::HeaderValue = http::HeaderValue::from_static("zstd");

#[derive(Clone, Copy, Debug)]
pub enum CompressionStrategy {
    None,
    #[cfg(feature = "compression")]
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
            #[cfg(not(target_arch = "wasm32"))]
            let result = zstd::Encoder::new(writer, level).and_then(|mut e| {
                e.write_all(&data)?;
                Ok((e.finish()?, strategy))
            });
            #[cfg(target_arch = "wasm32")]
            let result = zrip::FrameEncoder::new(writer, level)
                .map_err(std::io::Error::other)
                .and_then(|mut e| {
                    e.write_all(&data)?;
                    Ok((e.finish()?, strategy))
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

#[cfg(all(test, feature = "compression"))]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    fn decompress(data: &[u8]) -> std::io::Result<Vec<u8>> {
        zstd::decode_all(data)
    }

    #[cfg(target_arch = "wasm32")]
    fn decompress(data: &[u8]) -> std::io::Result<Vec<u8>> {
        zrip::decompress(data).map_err(std::io::Error::other)
    }

    #[test]
    fn zstd_compression_roundtrips() {
        let data = b"hello zstd".repeat(100);
        let (compressed, strategy) = compress(data.clone(), CompressionStrategy::Zstd { level: 1 });

        assert!(matches!(strategy, CompressionStrategy::Zstd { level: 1 }));
        assert_eq!(decompress(&compressed).unwrap(), data);
    }
}
