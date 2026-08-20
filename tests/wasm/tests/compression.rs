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
fn profiling_codecs_use_zrip() {
    use profiling_compressor::{
        Compressor, ObservationCodec, ZstdObservationCodec, ZstdProfileCodec,
    };

    let payload = b"hello profile".repeat(100);
    let mut compressor = Compressor::<ZstdProfileCodec>::try_new(256, 4096, 1).unwrap();
    compressor.write_all(&payload).unwrap();
    let compressed = compressor.finish().unwrap();
    assert_eq!(zrip::decompress(&compressed).unwrap(), payload);

    let mut encoder = ZstdObservationCodec::new_encoder(256, 4096).unwrap();
    encoder.write_all(&payload).unwrap();
    let mut decoder = ZstdObservationCodec::encoder_into_decoder(encoder).unwrap();
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded).unwrap();
    assert_eq!(decoded, payload);
}
