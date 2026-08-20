// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_arch = "wasm32")]

// The full profiling crate has unrelated native-only dependencies. The codec
// module is standalone, so compile its production source directly here.
#[allow(dead_code)]
#[path = "../../../libdd-profiling/src/profiles/compressor.rs"]
mod profiling_compressor;

use libdd_capabilities::{
    Bytes, HttpClientCapability, HttpError, Request, Response, SleepCapability,
};
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    send_with_retry, CompressionStrategy, RetryBackoffType, RetryStrategy,
};
use std::{
    cell::RefCell,
    future::{self, Future},
    io::{Read, Write},
    rc::Rc,
    time::Duration,
};

#[derive(Clone, Debug, Default)]
struct TestCapabilities {
    request: Rc<RefCell<Option<Request<Bytes>>>>,
}

impl HttpClientCapability for TestCapabilities {
    fn new_client() -> Self {
        Self::default()
    }

    fn new_without_connection_pooling() -> Self {
        Self::default()
    }

    fn request(
        &self,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<Response<Bytes>, HttpError>> {
        self.request.replace(Some(request));
        future::ready(Ok(Response::new(Bytes::new())))
    }
}

impl SleepCapability for TestCapabilities {
    fn new() -> Self {
        Self::default()
    }

    fn sleep(&self, _duration: Duration) -> impl Future<Output = ()> {
        future::pending()
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn compression_uses_zrip() {
    let payload = b"hello zstd".repeat(100);
    let capabilities = TestCapabilities::default();
    let endpoint = Endpoint {
        url: "http://localhost/v0.4/traces".parse().unwrap(),
        ..Endpoint::default()
    };
    let retry = RetryStrategy::new(0, 0, RetryBackoffType::Constant, None);

    let (_, attempts) = send_with_retry(
        &capabilities,
        &endpoint,
        payload.clone(),
        &http::HeaderMap::new(),
        &retry,
        CompressionStrategy::Zstd { level: 1 },
    )
    .await
    .unwrap();

    assert_eq!(attempts, 1);
    let request = capabilities.request.borrow_mut().take().unwrap();
    assert_eq!(request.headers()["content-encoding"], "zstd");
    assert_ne!(request.body().as_ref(), payload);
    assert_eq!(zrip::decompress(request.body()).unwrap(), payload);
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
