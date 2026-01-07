// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! File-based HTTP request dumping for testing and debugging.
//!
//! This module implements a local server (Unix domain socket on Unix,
//! named pipe on Windows) that captures raw HTTP requests and writes them to disk.
//!
//! This is primarily used for testing to validate the exact bytes sent over the wire.

use anyhow::Context;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

use super::utils::find_subsequence;

/// HTTP 200 OK response with no body
const HTTP_200_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";

/// Spawns a dump server that intercepts HTTP requests and writes them to a file
///
/// Returns the socket/pipe path that can be used as a unix:// or windows:// URI
///
/// # Arguments
/// * `output_path` - Where to write the captured HTTP request bytes
///
/// # Returns
/// The path to the Unix socket (on Unix) or named pipe (on Windows) that the server is listening on
#[cfg(unix)]
pub(crate) fn spawn_dump_server(output_path: PathBuf) -> anyhow::Result<PathBuf> {
    use tokio::net::UnixListener;

    // Create a temporary socket path with randomness to avoid collisions
    // Retry if the path already exists (highly unlikely with 64-bit random IDs)
    let socket_path = loop {
        let random_id: u64 = rand::random();
        let path = std::env::temp_dir().join(format!(
            "libdatadog_dump_{}_{:x}.sock",
            std::process::id(),
            random_id
        ));
        if !path.exists() {
            break path;
        }
    };

    let (tx, rx) = std::sync::mpsc::channel();
    let socket_path_for_thread = socket_path.clone();

    std::thread::spawn(move || {
        // Top-level error handler - all errors logged here
        let result = (|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async {
                let listener = UnixListener::bind(&socket_path_for_thread)?;
                tx.send(Ok(()))?;
                run_dump_server_unix(output_path, listener).await
            })
        })();

        if let Err(e) = result {
            eprintln!("[dump-server] Error: {}", e);
            let _ = tx.send(Err(e));
        }
    });

    // Wait for server to be ready
    rx.recv()??;
    Ok(socket_path)
}

/// Spawns a dump server that intercepts HTTP requests and writes them to a file
///
/// Returns the pipe path that can be used as a windows:// URI
///
/// # Arguments
/// * `output_path` - Where to write the captured HTTP request bytes
///
/// # Returns
/// The path to the Windows named pipe that the server is listening on
#[cfg(windows)]
pub(crate) fn spawn_dump_server(output_path: PathBuf) -> anyhow::Result<PathBuf> {
    use tokio::net::windows::named_pipe::ServerOptions;

    // Create a unique named pipe name with randomness to avoid collisions
    // With 64-bit random IDs, collision probability is ~1 in 18 quintillion
    let random_id: u64 = rand::random();
    let pipe_name = format!(
        r"\\.\pipe\libdatadog_dump_{}_{:x}",
        std::process::id(),
        random_id
    );
    let pipe_path = PathBuf::from(&pipe_name);

    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        // Top-level error handler - all errors logged here
        let result = (|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async {
                // Create the first pipe instance before signaling ready
                let first_server = ServerOptions::new()
                    .first_pipe_instance(true)
                    .create(&pipe_name)?;

                tx.send(Ok(()))?;
                run_dump_server_windows(output_path, pipe_name, first_server).await
            })
        })();

        if let Err(e) = result {
            eprintln!("[dump-server] Error: {}", e);
            let _ = tx.send(Err(e));
        }
    });

    // Wait for server to be ready
    rx.recv()??;
    Ok(pipe_path)
}

/// Async server loop for Unix sockets
#[cfg(unix)]
async fn run_dump_server_unix(
    output_path: PathBuf,
    listener: tokio::net::UnixListener,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        if let Err(e) = handle_connection(stream, output_path.clone()).await {
            eprintln!("[dump-server] Error handling connection: {:#}", e);
        }
    }
}

/// Async server loop for Windows named pipes
#[cfg(windows)]
async fn run_dump_server_windows(
    output_path: PathBuf,
    pipe_name: String,
    first_server: tokio::net::windows::named_pipe::NamedPipeServer,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut server = Some(first_server);

    loop {
        // Use the first_server for the first iteration, then create new instances
        let current_server = match server.take() {
            Some(s) => s,
            None => ServerOptions::new()
                .first_pipe_instance(false)
                .create(&pipe_name)?,
        };

        // Wait for client connection
        current_server.connect().await?;

        // Handle connection sequentially (this is just a debugging API)
        if let Err(e) = handle_connection(current_server, output_path.clone()).await {
            eprintln!("[dump-server] Error handling connection: {:#}", e);
        }
    }
}

/// Check if headers indicate chunked transfer encoding
fn is_chunked_encoding(headers: &[httparse::Header]) -> bool {
    headers.iter().any(|h| {
        h.name.eq_ignore_ascii_case("transfer-encoding")
            && std::str::from_utf8(h.value).is_ok_and(|v| v.to_lowercase().contains("chunked"))
    })
}

/// Extract Content-Length from headers
fn get_content_length(headers: &[httparse::Header]) -> Option<usize> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-length"))
        .and_then(|h| std::str::from_utf8(h.value).ok())
        .and_then(|v| v.trim().parse().ok())
}

/// Parse HTTP request headers and extract metadata
/// Returns (headers_end_position, content_length, is_chunked)
fn parse_http_headers(request_data: &[u8]) -> anyhow::Result<(Option<usize>, Option<usize>, bool)> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);

    match req.parse(request_data)? {
        httparse::Status::Complete(headers_len) => {
            let content_length = get_content_length(req.headers);
            let is_chunked = is_chunked_encoding(req.headers);
            Ok((Some(headers_len), content_length, is_chunked))
        }
        httparse::Status::Partial => Ok((None, None, false)),
    }
}

/// Check if we have received a complete HTTP request
fn is_request_complete(
    request_data: &[u8],
    headers_end_pos: Option<usize>,
    content_length: Option<usize>,
    is_chunked: bool,
) -> bool {
    let Some(headers_end) = headers_end_pos else {
        return false;
    };

    // Check Content-Length based completion
    if let Some(expected_len) = content_length {
        let body_len = request_data.len() - headers_end;
        return body_len >= expected_len;
    }

    // Check chunked transfer encoding completion (ends with 0\r\n\r\n)
    if is_chunked {
        let body = &request_data[headers_end..];
        return body.ends_with(b"0\r\n\r\n");
    }

    false
}

/// Parsed HTTP request with raw data
struct ParsedRequest {
    raw_data: Vec<u8>,
    headers_len: usize,
    is_chunked: bool,
}

/// Read complete HTTP request from an async stream
async fn read_http_request<R: AsyncReadExt + Unpin>(
    stream: &mut R,
) -> anyhow::Result<ParsedRequest> {
    let mut request_data = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut content_length: Option<usize> = None;
    let mut headers_end_pos: Option<usize> = None;
    let mut is_chunked = false;

    loop {
        let n = stream
            .read(&mut buffer)
            .await
            .context("Failed to read from connection")?;

        if n == 0 {
            break; // Connection closed
        }

        request_data.extend_from_slice(&buffer[..n]);

        // Parse headers if we haven't completed parsing yet
        if headers_end_pos.is_none() {
            (headers_end_pos, content_length, is_chunked) = parse_http_headers(&request_data)?;
        }

        // Check if we have the complete request
        if is_request_complete(&request_data, headers_end_pos, content_length, is_chunked) {
            break;
        }
    }

    Ok(ParsedRequest {
        raw_data: request_data,
        headers_len: headers_end_pos.unwrap_or(0),
        is_chunked,
    })
}

/// Decode chunked transfer encoding
fn decode_chunked_body(chunked_data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut pos = 0;

    while pos < chunked_data.len() {
        // Find the end of the chunk size line (\r\n)
        let line_end = find_subsequence(&chunked_data[pos..], b"\r\n")
            .with_context(|| format!("Missing CRLF after chunk size at position {}", pos))?;

        // Parse the chunk size (hex)
        let size_str = std::str::from_utf8(&chunked_data[pos..pos + line_end])
            .context("Invalid UTF-8 in chunk size")?;

        let chunk_size = usize::from_str_radix(size_str.trim(), 16)
            .with_context(|| format!("Invalid hex chunk size: {:?}", size_str))?;

        if chunk_size == 0 {
            break; // End of chunks
        }

        // Move past the size line and \r\n
        pos += line_end + 2;

        // Read the chunk data
        let remaining = chunked_data.len() - pos;
        anyhow::ensure!(
            chunk_size <= remaining,
            "Incomplete chunk data: expected {chunk_size} bytes at position {pos}, only {remaining} bytes remaining"
        );

        result.extend_from_slice(&chunked_data[pos..pos + chunk_size]);
        pos += chunk_size;

        // Skip the trailing \r\n after the chunk
        if pos + 2 <= chunked_data.len() && &chunked_data[pos..pos + 2] == b"\r\n" {
            pos += 2;
        }
    }

    Ok(result)
}

/// Reconstruct HTTP request with chunked encoding decoded
fn reconstruct_with_content_length(
    request_data: &[u8],
    headers_len: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);

    // Parse the request
    match req.parse(request_data)? {
        httparse::Status::Complete(_) => {}
        httparse::Status::Partial => anyhow::bail!("Incomplete HTTP request for reconstruction"),
    }

    let body = &request_data[headers_len..];
    let decoded_body = decode_chunked_body(body)?;
    let mut reconstructed = Vec::new();

    // Reconstruct request line
    if let (Some(method), Some(path), Some(version)) = (req.method, req.path, req.version) {
        let line = format!("{} {} HTTP/1.{}\r\n", method, path, version);
        reconstructed.extend_from_slice(line.as_bytes());
    }

    // Add all headers except Transfer-Encoding
    for header in req.headers {
        if !header.name.eq_ignore_ascii_case("transfer-encoding") {
            reconstructed.extend_from_slice(header.name.as_bytes());
            reconstructed.extend_from_slice(b": ");
            reconstructed.extend_from_slice(header.value);
            reconstructed.extend_from_slice(b"\r\n");
        }
    }

    // Add Content-Length header and body
    let cl_header = format!("Content-Length: {}\r\n\r\n", decoded_body.len());
    reconstructed.extend_from_slice(cl_header.as_bytes());
    reconstructed.extend_from_slice(&decoded_body);

    Ok(reconstructed)
}

/// Write request data to file if non-empty
/// Decodes chunked transfer encoding if present
async fn write_request_to_file(
    output_path: &PathBuf,
    parsed_request: &ParsedRequest,
) -> anyhow::Result<()> {
    if parsed_request.raw_data.is_empty() {
        return Ok(());
    }

    let data_to_write = if parsed_request.is_chunked && parsed_request.headers_len > 0 {
        reconstruct_with_content_length(&parsed_request.raw_data, parsed_request.headers_len)?
    } else {
        parsed_request.raw_data.clone()
    };

    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(output_path)
        .await
        .context("Failed to create dump file")?;

    file.write_all(&data_to_write)
        .await
        .context("Failed to write request dump")?;

    // Sync to ensure data is persisted to disk before sending HTTP response
    file.sync_all()
        .await
        .context("Failed to sync request dump to disk")?;

    Ok(())
}

/// Handle a connection: read HTTP request, write to file, send response
async fn handle_connection<S>(mut stream: S, output_path: PathBuf) -> anyhow::Result<()>
where
    S: AsyncReadExt + tokio::io::AsyncWriteExt + Unpin,
{
    let parsed_request = read_http_request(&mut stream).await?;
    write_request_to_file(&output_path, &parsed_request).await?;

    stream
        .write_all(HTTP_200_RESPONSE)
        .await
        .context("Failed to send HTTP response")?;

    Ok(())
}
