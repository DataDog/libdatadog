// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Local agent for the exporter: a loopback port served by its own thread. Shared by the
//! harness and the `profile_sequence` fuzz target.

/// Reads one HTTP/1.1 request (`Content-Length` or chunked body) and answers 202.
fn serve_request(mut stream: std::net::TcpStream) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut request = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let mut body_start = None;
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
        if body_start.is_none() {
            body_start = request
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4);
        }
        let Some(start) = body_start else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..start]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok());
        let done = match content_length {
            Some(len) => request.len() - start >= len,
            None if headers.contains("transfer-encoding: chunked") => {
                request.ends_with(b"0\r\n\r\n")
            }
            None => true,
        };
        if done {
            break;
        }
    }
    stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

/// A local agent on a loopback port, answering every request on its own thread. Its
/// allocations happen on that thread: the harness reports them, it does not enumerate them.
pub fn local_agent() -> &'static str {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the local agent");
    let port = listener.local_addr().expect("local agent address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve_request(stream);
        }
    });
    Box::leak(format!("http://127.0.0.1:{port}").into_boxed_str())
}
