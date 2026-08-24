// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_arch = "wasm32")]

// These modules are standalone, while their parent crates have unrelated
// native-only dependencies. Compile the production source directly here.
#[allow(dead_code)]
#[path = "../../../libdd-trace-utils/src/send_with_retry/compression.rs"]
mod trace_compression;

#[allow(dead_code)]
#[path = "../../../libdd-profiling/src/profiles/compressor.rs"]
mod profiling_compressor;

use std::io::{Read, Write};

fn compress_profile(payload: &[u8], level: i32) -> std::io::Result<Vec<u8>> {
    use profiling_compressor::{Compressor, ZstdProfileCodec};

    let mut compressor = Compressor::<ZstdProfileCodec>::try_new(256, 4096, level)?;
    compressor.write_all(payload)?;
    compressor.finish()
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn trace_compression_uses_zrip() {
    use trace_compression::{add_headers, compress, CompressionStrategy};

    let payload = b"hello zstd".repeat(100);
    let (compressed, strategy) = compress(payload.clone(), CompressionStrategy::Zstd { level: 1 });
    assert_eq!(zrip::decompress(&compressed).unwrap(), payload);

    let mut headers = http::HeaderMap::new();
    add_headers(&mut headers, strategy);
    assert_eq!(headers["content-encoding"], "zstd");
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn trace_compression_levels_are_clamped() {
    use trace_compression::{compress, CompressionStrategy};

    let payload = b"hello zstd".repeat(100);
    for level in [-7, 4] {
        let (compressed, strategy) = compress(payload.clone(), CompressionStrategy::Zstd { level });
        assert!(matches!(strategy, CompressionStrategy::Zstd { level: actual } if actual == level));
        assert_eq!(zrip::decompress(&compressed).unwrap(), payload);
    }
    for (level, expected) in [(-8, -7), (5, 4), (22, 4)] {
        let (compressed, strategy) = compress(payload.clone(), CompressionStrategy::Zstd { level });
        assert!(
            matches!(strategy, CompressionStrategy::Zstd { level: actual } if actual == expected)
        );
        assert_eq!(zrip::decompress(&compressed).unwrap(), payload);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn trace_compression_zero_uses_level_three() {
    use trace_compression::{compress, CompressionStrategy};

    let payload = b"hello zstd".repeat(100);
    let (default_compressed, strategy) =
        compress(payload.clone(), CompressionStrategy::Zstd { level: 0 });
    let (level_three_compressed, _) = compress(payload, CompressionStrategy::Zstd { level: 3 });

    assert_eq!(default_compressed, level_three_compressed);
    assert!(matches!(strategy, CompressionStrategy::Zstd { level: 3 }));
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn profiling_codecs_use_zrip() {
    use profiling_compressor::{ObservationCodec, ZstdObservationCodec};

    let payload = b"hello profile".repeat(100);
    let compressed = compress_profile(&payload, 1).unwrap();
    assert_eq!(zrip::decompress(&compressed).unwrap(), payload);

    let mut encoder = ZstdObservationCodec::new_encoder(256, 4096).unwrap();
    encoder.write_all(&payload).unwrap();
    let mut decoder = ZstdObservationCodec::encoder_into_decoder(encoder).unwrap();
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded).unwrap();
    assert_eq!(decoded, payload);
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn profiling_compression_levels_are_clamped() {
    let payload = b"hello profile".repeat(100);
    for level in [-7, 4] {
        let compressed = compress_profile(&payload, level).unwrap();
        assert_eq!(zrip::decompress(&compressed).unwrap(), payload);
    }
    for (level, expected) in [(-8, -7), (5, 4), (22, 4)] {
        assert_eq!(
            compress_profile(&payload, level).unwrap(),
            compress_profile(&payload, expected).unwrap()
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn profiling_compression_zero_uses_level_three() {
    let payload = b"hello profile".repeat(100);

    assert_eq!(
        compress_profile(&payload, 0).unwrap(),
        compress_profile(&payload, 3).unwrap()
    );
}
