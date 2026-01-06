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
        if let Err(e) = handle_connection_async(stream, output_path.clone()).await {
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
        if let Err(e) = handle_connection_async(current_server, output_path.clone()).await {
            eprintln!("[dump-server] Error handling connection: {:#}", e);
        }
    }
}

/// Helper function to find a subsequence in a byte slice
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Parse Content-Length from HTTP headers
fn parse_content_length(headers_data: &[u8]) -> Option<usize> {
    if let Ok(headers_str) = std::str::from_utf8(headers_data) {
        for line in headers_str.lines() {
            if line.to_lowercase().starts_with("content-length:") {
                if let Some(len_str) = line.split(':').nth(1) {
                    return len_str.trim().parse().ok();
                }
            }
        }
    }
    None
}

/// Check if we have received a complete HTTP request
fn is_request_complete(
    request_data: &[u8],
    headers_end_pos: Option<usize>,
    content_length: Option<usize>,
) -> bool {
    if let Some(headers_end) = headers_end_pos {
        if let Some(expected_len) = content_length {
            let body_len = request_data.len() - headers_end;
            return body_len >= expected_len;
        }

        // For chunked transfer encoding, look for the end chunk marker
        // The end of a chunked body is: 0\r\n\r\n
        if request_data.len() >= headers_end + 5 {
            let body = &request_data[headers_end..];
            // Check if body ends with the chunked encoding terminator
            if body.ends_with(b"0\r\n\r\n") {
                return true;
            }
        }
    }
    false
}

/// Read complete HTTP request from an async stream
async fn read_http_request_async<R: tokio::io::AsyncReadExt + Unpin>(
    stream: &mut R,
) -> anyhow::Result<Vec<u8>> {
    let mut request_data = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut content_length: Option<usize> = None;
    let mut headers_end_pos: Option<usize> = None;

    loop {
        let n = stream
            .read(&mut buffer)
            .await
            .context("Failed to read from connection")?;

        if n == 0 {
            break; // Connection closed
        }

        request_data.extend_from_slice(&buffer[..n]);

        // Look for end of headers if we haven't found it yet
        if headers_end_pos.is_none() {
            if let Some(pos) = find_subsequence(&request_data, b"\r\n\r\n") {
                headers_end_pos = Some(pos + 4);
                content_length = parse_content_length(&request_data[..pos]);
            }
        }

        // Check if we have the complete request
        if is_request_complete(&request_data, headers_end_pos, content_length) {
            break;
        }
    }

    Ok(request_data)
}

/// Decode chunked transfer encoding
fn decode_chunked_body(chunked_data: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut pos = 0;

    while pos < chunked_data.len() {
        // Find the end of the chunk size line (\r\n)
        let Some(line_end) = find_subsequence(&chunked_data[pos..], b"\r\n") else {
            break;
        };

        // Parse the chunk size (hex)
        let Ok(size_str) = std::str::from_utf8(&chunked_data[pos..pos + line_end]) else {
            break;
        };

        let Ok(chunk_size) = usize::from_str_radix(size_str.trim(), 16) else {
            break;
        };

        if chunk_size == 0 {
            break; // End of chunks
        }

        // Move past the size line and \r\n
        pos += line_end + 2;

        // Read the chunk data
        if pos + chunk_size > chunked_data.len() {
            break;
        }

        result.extend_from_slice(&chunked_data[pos..pos + chunk_size]);
        pos += chunk_size;

        // Skip the trailing \r\n after the chunk
        if pos + 2 <= chunked_data.len() && &chunked_data[pos..pos + 2] == b"\r\n" {
            pos += 2;
        }
    }

    result
}

/// Write request data to file if non-empty (async version)
/// Decodes chunked transfer encoding if present
async fn write_request_to_file_async(
    output_path: &PathBuf,
    request_data: &[u8],
) -> anyhow::Result<()> {
    if request_data.is_empty() {
        return Ok(());
    }

    // Check if this is a chunked request and decode it
    let data_to_write = if let Some(headers_end) = find_subsequence(request_data, b"\r\n\r\n") {
        let headers = &request_data[..headers_end];
        let body = &request_data[headers_end + 4..];

        // Check for transfer-encoding: chunked
        let is_chunked = std::str::from_utf8(headers).is_ok_and(|headers_str| {
            headers_str
                .to_lowercase()
                .contains("transfer-encoding: chunked")
        });

        if is_chunked {
            // Decode the chunked body and reconstruct the request with Content-Length
            let decoded_body = decode_chunked_body(body);
            let mut reconstructed = Vec::new();

            // Add headers but replace transfer-encoding with content-length
            if let Ok(headers_str) = std::str::from_utf8(headers) {
                for line in headers_str.lines() {
                    if !line.to_lowercase().starts_with("transfer-encoding:") {
                        reconstructed.extend_from_slice(line.as_bytes());
                        reconstructed.extend_from_slice(b"\r\n");
                    }
                }
                // Add content-length header
                reconstructed.extend_from_slice(
                    format!("Content-Length: {}\r\n", decoded_body.len()).as_bytes(),
                );
            }

            // Add the decoded body
            reconstructed.extend_from_slice(b"\r\n");
            reconstructed.extend_from_slice(&decoded_body);

            reconstructed
        } else {
            request_data.to_vec()
        }
    } else {
        request_data.to_vec()
    };

    // Write to file and explicitly sync to ensure data is on disk before responding
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
async fn handle_connection_async<S>(mut stream: S, output_path: PathBuf) -> anyhow::Result<()>
where
    S: tokio::io::AsyncReadExt + tokio::io::AsyncWriteExt + Unpin,
{
    let request_data = read_http_request_async(&mut stream).await?;
    write_request_to_file_async(&output_path, &request_data).await?;

    stream
        .write_all(HTTP_200_RESPONSE)
        .await
        .context("Failed to send HTTP response")?;

    Ok(())
}
