#[cfg(any(unix, windows))]
use std::path::PathBuf;

/// HTTP 200 OK response with no body
#[cfg(any(unix, windows))]
const HTTP_200_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";

/// Async server loop for Unix sockets
#[cfg(unix)]
async fn run_dump_server_unix(
    output_path: PathBuf,
    listener: tokio::net::UnixListener,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        handle_connection_async(stream, output_path.clone()).await;
    }
}

/// Spawns a HTTP dump server that saves incoming requests to a file
/// Returns the Unix socket path that the server is listening on
#[cfg(unix)]
pub(crate) fn spawn_dump_server(output_path: PathBuf) -> anyhow::Result<PathBuf> {
    use tokio::net::UnixListener;

    // Create a temporary socket path with randomness to avoid collisions
    let random_id: u64 = rand::random();
    let socket_path = std::env::temp_dir().join(format!(
        "libdatadog_dump_{}_{:x}.sock",
        std::process::id(),
        random_id
    ));

    // Remove socket file if it already exists
    let _ = std::fs::remove_file(&socket_path);

    let socket_path_clone = socket_path.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    
    std::thread::spawn(move || {
        // Top-level error handler - all errors logged here
        let result = (|| -> anyhow::Result<()> {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let listener = UnixListener::bind(&socket_path)?;
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
    Ok(socket_path_clone)
}

/// Async server loop for Windows named pipes
#[cfg(windows)]
async fn run_dump_server_windows(output_path: PathBuf, pipe_name: String) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    loop {
        // Create server instance
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)?;

        // Wait for client connection
        server.connect().await?;

        // Handle connection sequentially (this is just a debugging API)
        handle_connection_async(server, output_path.clone()).await;
    }
}

/// Spawns a HTTP dump server that saves incoming requests to a file
/// Returns the named pipe path that the server is listening on
#[cfg(windows)]
pub(crate) fn spawn_dump_server(output_path: PathBuf) -> anyhow::Result<PathBuf> {
    // Create a unique named pipe name with randomness to avoid collisions
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
        let result = (|| -> anyhow::Result<()> {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                tx.send(Ok(()))?;
                run_dump_server_windows(output_path, pipe_name).await
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

/// Helper function to find a subsequence in a byte slice
#[cfg(any(unix, windows))]
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Parse Content-Length from HTTP headers
#[cfg(any(unix, windows))]
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
#[cfg(any(unix, windows))]
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
    }
    false
}

/// Read complete HTTP request from an async stream
#[cfg(any(unix, windows))]
async fn read_http_request_async<R: tokio::io::AsyncReadExt + Unpin>(
    stream: &mut R,
) -> Vec<u8> {
    let mut request_data = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut content_length: Option<usize> = None;
    let mut headers_end_pos: Option<usize> = None;

    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => break, // Connection closed
            Ok(n) => {
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
            Err(e) => {
                eprintln!("[dump-server] Failed to read from connection: {}", e);
                break;
            }
        }
    }

    request_data
}

/// Write request data to file if non-empty (async version)
#[cfg(any(unix, windows))]
async fn write_request_to_file_async(output_path: &PathBuf, request_data: &[u8]) {
    if !request_data.is_empty() {
        if let Err(e) = tokio::fs::write(output_path, request_data).await {
            eprintln!("[dump-server] Failed to write request dump to {:?}: {}", output_path, e);
        }
    }
}

/// Handle a connection: read HTTP request, write to file, send response
#[cfg(any(unix, windows))]
async fn handle_connection_async<S>(mut stream: S, output_path: PathBuf)
where
    S: tokio::io::AsyncReadExt + tokio::io::AsyncWriteExt + Unpin,
{
    let request_data = read_http_request_async(&mut stream).await;
    write_request_to_file_async(&output_path, &request_data).await;
    
    if let Err(e) = stream.write_all(HTTP_200_RESPONSE).await {
        eprintln!("[dump-server] Failed to send HTTP response: {}", e);
    }
}
