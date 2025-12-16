#[cfg(unix)]
use std::path::PathBuf;

/// Spawns a HTTP dump server that saves incoming requests to a file
/// Returns the Unix socket path that the server is listening on
#[cfg(unix)]
pub(crate) fn spawn_dump_server(output_path: PathBuf) -> anyhow::Result<PathBuf> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    use anyhow::Context;

    // Create a temporary socket path with randomness to avoid collisions
    let random_id: u64 = rand::random();
    let socket_path = std::env::temp_dir().join(format!(
        "libdatadog_dump_{}_{:x}.sock",
        std::process::id(),
        random_id
    ));

    // Remove socket file if it already exists
    let _ = std::fs::remove_file(&socket_path);

    let listener =
        UnixListener::bind(&socket_path).context("Failed to bind Unix socket for dump server")?;

    let socket_path_clone = socket_path.clone();

    // Spawn the server thread
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let output_path = output_path.clone();

                    std::thread::spawn(move || {
                        // Read the HTTP request in chunks
                        let mut request_data = Vec::new();
                        let mut buffer = [0u8; 8192];
                        let mut content_length: Option<usize> = None;
                        let mut headers_end_pos: Option<usize> = None;

                        // Read headers first
                        loop {
                            match stream.read(&mut buffer) {
                                Ok(0) => break, // Connection closed
                                Ok(n) => {
                                    request_data.extend_from_slice(&buffer[..n]);

                                    // Look for end of headers if we haven't found it yet
                                    if headers_end_pos.is_none() {
                                        if let Some(pos) =
                                            find_subsequence(&request_data, b"\r\n\r\n")
                                        {
                                            headers_end_pos = Some(pos + 4);

                                            // Parse Content-Length from headers
                                            if let Ok(headers_str) =
                                                std::str::from_utf8(&request_data[..pos])
                                            {
                                                for line in headers_str.lines() {
                                                    if line
                                                        .to_lowercase()
                                                        .starts_with("content-length:")
                                                    {
                                                        if let Some(len_str) =
                                                            line.split(':').nth(1)
                                                        {
                                                            content_length =
                                                                len_str.trim().parse().ok();
                                                        }
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Check if we have the complete request
                                    if let Some(headers_end) = headers_end_pos {
                                        if let Some(expected_len) = content_length {
                                            let body_len = request_data.len() - headers_end;
                                            if body_len >= expected_len {
                                                break; // Complete request received
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Failed to read from dump server socket: {}", e);
                                    break;
                                }
                            }
                        }

                        if !request_data.is_empty() {
                            // Write the request directly to the specified file
                            if let Err(e) = std::fs::write(&output_path, &request_data) {
                                eprintln!(
                                    "Failed to write request dump to {:?}: {}",
                                    output_path, e
                                );
                            }
                        }

                        // Send a simple HTTP 200 response
                        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                        let _ = stream.write_all(response);
                    });
                }
                Err(e) => {
                    eprintln!("Failed to accept connection on dump server: {}", e);
                    break;
                }
            }
        }
    });

    Ok(socket_path_clone)
}

/// Helper function to find a subsequence in a byte slice
#[cfg(unix)]
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
