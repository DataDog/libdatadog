// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The proxy environment is read once when the first HTTP connector is constructed.
//! Keep this test in its own process so it controls that initial environment.

use core::net::SocketAddr;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use libdd_crashtracker::{CrashInfoBuilder, ErrorKind};

fn spawn_connect_proxy() -> (SocketAddr, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let mut stream = stream.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];

            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let length = stream.read(&mut buffer).unwrap();
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..length]);
            }

            let request = String::from_utf8(request).unwrap();
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .unwrap()
                .to_owned();
            sender.send(target).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
        }
    });

    (address, receiver)
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn direct_crash_requests_use_https_proxy() -> anyhow::Result<()> {
    let (proxy_address, targets) = spawn_connect_proxy();

    std::env::set_var("_DD_DIRECT_SUBMISSION_ENABLED", "true");
    std::env::set_var("DD_API_KEY", "test-api-key");
    std::env::set_var(
        "DD_APM_TELEMETRY_DD_URL",
        "https://telemetry.example.invalid",
    );
    std::env::set_var("DD_TRACE_AGENT_URL", "https://telemetry.example.invalid");
    std::env::set_var("DD_ERRORS_INTAKE_DD_URL", "https://errors.example.invalid");
    std::env::set_var("HTTPS_PROXY", format!("http://{proxy_address}"));
    std::env::set_var("NO_PROXY", "");
    std::env::remove_var("REQUEST_METHOD");

    let mut builder = CrashInfoBuilder::new();
    builder.with_kind(ErrorKind::UnixSignal)?;
    builder.build()?.async_upload_to_endpoint(&None).await?;

    let mut targets = [targets.try_recv()?, targets.try_recv()?];
    targets.sort();

    assert_eq!(
        targets,
        [
            "errors.example.invalid:443",
            "telemetry.example.invalid:443",
        ]
    );

    Ok(())
}
