// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(not(target_arch = "wasm32"))]

use libdd_capabilities::http::ResponseBodyLimit as CapabilityResponseBodyLimit;
use libdd_capabilities::{HttpClientCapability, HttpError};
use libdd_capabilities_impl::{NativeHttpClient, ResponseBodyLimit};
use std::io::{BufRead, BufReader, Write};

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn both_limit_types_bound_buffered_and_streamed_responses() {
    for streamed in [false, true] {
        for (native, capability, accepted) in [
            (None, None, true),
            (Some(8), None, true),
            (None, Some(8), true),
            (Some(4), None, false),
            (None, Some(4), false),
            (Some(4), Some(8), false),
            (Some(8), Some(4), false),
            (Some(0), Some(8), false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                reader.into_inner().write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678"
                ).unwrap();
            });

            let client = NativeHttpClient::new_client();
            let mut request = http::Request::builder().uri(format!("http://{address}/"));
            if let Some(limit) = native {
                request = request.extension(ResponseBodyLimit(limit));
            }
            if let Some(limit) = capability {
                request = request.extension(CapabilityResponseBodyLimit(limit));
            }
            let result = if streamed {
                let (sender, response) = client.request_streamed(request.body(()).unwrap());
                drop(sender);
                response.await
            } else {
                client
                    .request(request.body(bytes::Bytes::new()).unwrap())
                    .await
            };
            server.join().unwrap();
            if accepted {
                assert_eq!(result.unwrap().body().as_ref(), b"12345678");
            } else {
                assert!(
                    matches!(result, Err(HttpError::ResponseBody(_))),
                    "{native:?}, {capability:?}, streamed={streamed}"
                );
            }
        }
    }
}
