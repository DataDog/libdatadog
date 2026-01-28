// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for ProfileExporter
//!
//! These tests validate the full export flow across different endpoint types.

mod common;

use libdd_profiling::exporter::config;
use libdd_profiling::exporter::utils::parse_http_request;
use libdd_profiling::exporter::ProfileExporter;
use libdd_profiling::internal::EncodedProfile;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Shared state for test HTTP servers
#[derive(Debug, Clone)]
struct ReceivedRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Helper to create a unique temp file path
fn create_temp_file_path(extension: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "libdd_test_{}_{:x}.{}",
        std::process::id(),
        rand::random::<u64>(),
        extension
    ))
}

/// Transport type for endpoint tests
enum Transport {
    Tcp,
    #[cfg(unix)]
    UnixSocket,
    #[cfg(windows)]
    NamedPipe,
}

/// Server info returned from spawning a test server
struct ServerInfo {
    port: Option<u16>,
    #[cfg(unix)]
    socket_path: Option<PathBuf>,
    #[cfg(windows)]
    pipe_path: Option<PathBuf>,
    received_requests: Arc<Mutex<Vec<ReceivedRequest>>>,
}

/// Spawn an async HTTP server with the specified transport
async fn spawn_server(transport: Transport) -> anyhow::Result<ServerInfo> {
    let received_requests = Arc::new(Mutex::new(Vec::new()));
    let requests_clone = received_requests.clone();

    match transport {
        Transport::Tcp => {
            use tokio::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let port = listener.local_addr()?.port();

            tokio::spawn(async move {
                if let Ok((stream, _)) = listener.accept().await {
                    read_and_capture_request(stream, requests_clone).await;
                }
            });

            Ok(ServerInfo {
                port: Some(port),
                #[cfg(unix)]
                socket_path: None,
                #[cfg(windows)]
                pipe_path: None,
                received_requests,
            })
        }

        #[cfg(unix)]
        Transport::UnixSocket => {
            use tokio::net::UnixListener;

            let socket_path = create_temp_file_path("sock");
            let _ = std::fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path)?;

            tokio::spawn(async move {
                if let Ok((stream, _)) = listener.accept().await {
                    read_and_capture_request(stream, requests_clone).await;
                }
            });

            Ok(ServerInfo {
                port: None,
                socket_path: Some(socket_path),
                #[cfg(windows)]
                pipe_path: None,
                received_requests,
            })
        }

        #[cfg(windows)]
        Transport::NamedPipe => {
            use tokio::net::windows::named_pipe::ServerOptions;

            // Create a unique named pipe name
            let pipe_name = format!(
                r"\\.\pipe\dd_test_{}_{:x}",
                std::process::id(),
                rand::random::<u64>()
            );
            let pipe_path = PathBuf::from(&pipe_name);

            // Create server endpoint
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)?;

            tokio::spawn(async move {
                // Wait for a client to connect
                if server.connect().await.is_ok() {
                    read_and_capture_request(server, requests_clone).await;
                }
            });

            // Give the server a moment to start listening
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

            Ok(ServerInfo {
                port: None,
                #[cfg(unix)]
                socket_path: None,
                pipe_path: Some(pipe_path),
                received_requests,
            })
        }
    }
}

/// Read HTTP request from an async stream and capture it
async fn read_and_capture_request<S>(
    mut stream: S,
    received_requests: Arc<Mutex<Vec<ReceivedRequest>>>,
) where
    S: tokio::io::AsyncReadExt + tokio::io::AsyncWriteExt + Unpin,
{
    let mut buffer = Vec::new();
    let mut temp_buf = [0u8; 8192];
    let mut headers_complete = false;
    let mut content_length: Option<usize> = None;
    let mut headers_end_pos: Option<usize> = None;

    loop {
        match stream.read(&mut temp_buf).await {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&temp_buf[..n]);

                if !headers_complete {
                    if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                        headers_end_pos = Some(pos + 4);
                        headers_complete = true;

                        if let Ok(header_str) = std::str::from_utf8(&buffer[..pos]) {
                            for line in header_str.lines() {
                                if line.to_lowercase().starts_with("content-length:") {
                                    if let Some(len_str) = line.split(':').nth(1) {
                                        content_length = len_str.trim().parse().ok();
                                    }
                                }
                            }
                        }
                    }
                }

                if let (Some(headers_end), Some(expected_len)) = (headers_end_pos, content_length) {
                    if buffer.len() - headers_end >= expected_len {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }

    if let Ok(req) = parse_http_request(&buffer) {
        received_requests.lock().unwrap().push(ReceivedRequest {
            method: req.method,
            path: req.path,
            headers: req.headers,
            body: req.body,
        });
    }

    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    let _ = stream.write_all(response).await;
}

/// Result source for capturing the HTTP request
enum RequestSource {
    File(PathBuf),
    Captured(Arc<Mutex<Vec<ReceivedRequest>>>),
}

/// Export a comprehensive profile with all features and capture the HTTP request
async fn export_full_profile(
    endpoint: libdd_common::Endpoint,
    source: RequestSource,
) -> anyhow::Result<ReceivedRequest> {
    // Build tags
    let tags = vec![
        libdd_common::tag::Tag::new("service", "test-service")?,
        libdd_common::tag::Tag::new("env", "test")?,
    ];

    let additional_tags = vec![
        libdd_common::tag::Tag::new("runtime", "rust")?,
        libdd_common::tag::Tag::new("version", "1.0")?,
    ];

    // Build additional files
    let additional_files = common::create_test_additional_files();

    // Build metadata
    let internal_metadata = serde_json::json!({
        "no_signals_workaround_enabled": "true",
        "execution_trace_enabled": "false",
        "custom_field": "custom_value"
    });

    let info = serde_json::json!({
        "application": {
            "start_time": "2024-01-01T00:00:00Z",
            "env": "production"
        },
        "platform": {
            "hostname": "test-host",
            "kernel": "Linux 5.10"
        },
        "runtime": {
            "engine": "rust",
            "version": "1.75.0"
        }
    });

    // Create exporter and send
    let exporter = ProfileExporter::new("test-lib", "1.0.0", "native", tags, endpoint)?;
    let profile = EncodedProfile::test_instance()?;

    exporter
        .send(
            profile,
            &additional_files,
            &additional_tags,
            Some(internal_metadata),
            Some(info),
            Some("entrypoint.name:main,pid:12345"),
            None,
        )
        .await?;

    // Get the request from the appropriate source
    match source {
        RequestSource::File(path) => {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            let request_bytes = std::fs::read(&path)?;
            let req = parse_http_request(&request_bytes)?;
            Ok(ReceivedRequest {
                method: req.method,
                path: req.path,
                headers: req.headers,
                body: req.body,
            })
        }
        RequestSource::Captured(requests) => {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let reqs = requests.lock().unwrap();
            if reqs.is_empty() {
                anyhow::bail!("No request captured");
            }
            Ok(reqs[0].clone())
        }
    }
}

/// Validate the full export result
fn validate_full_export(req: &ReceivedRequest, expected_path: &str) -> anyhow::Result<()> {
    // Verify request basics
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, expected_path);

    // Verify container/entity ID headers if available on the system
    // These should be consistently present or absent
    let has_container_id = req.headers.contains_key("datadog-container-id");
    let has_entity_id = req.headers.contains_key("datadog-entity-id");
    assert_eq!(
        has_container_id, has_entity_id,
        "Container ID and Entity ID headers should both be present or both absent"
    );

    // If present, verify they match system values
    if let Some(container_id) = libdd_common::entity_id::get_container_id() {
        assert_eq!(
            req.headers.get("datadog-container-id").unwrap(),
            container_id,
            "Container ID header should match system container ID"
        );
    }

    if let Some(entity_id) = libdd_common::entity_id::get_entity_id() {
        assert_eq!(
            req.headers.get("datadog-entity-id").unwrap(),
            entity_id,
            "Entity ID header should match system entity ID"
        );
    }

    // Parse the request to get multipart parts
    // We need to reconstruct a minimal HTTP request to parse
    let mut http_request_bytes = Vec::new();
    http_request_bytes
        .extend_from_slice(format!("{} {} HTTP/1.1\r\n", req.method, req.path).as_bytes());
    for (key, value) in &req.headers {
        http_request_bytes.extend_from_slice(format!("{}: {}\r\n", key, value).as_bytes());
    }
    http_request_bytes.extend_from_slice(b"\r\n");
    http_request_bytes.extend_from_slice(&req.body);

    let parsed_req = parse_http_request(&http_request_bytes)?;
    let parts = &parsed_req.multipart_parts;

    // Find event JSON
    let event_part = parts
        .iter()
        .find(|p| p.name == "event")
        .ok_or_else(|| anyhow::anyhow!("Missing event part"))?;
    let event_json: serde_json::Value = serde_json::from_slice(&event_part.content)?;

    // Verify basic event fields
    assert_eq!(event_json["family"], "native");
    assert_eq!(event_json["version"], "4");

    // Verify tags (base + additional)
    let tags_profiler = event_json["tags_profiler"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing tags_profiler"))?;
    for tag in &[
        "service:test-service",
        "env:test",
        "runtime:rust",
        "version:1.0",
    ] {
        assert!(tags_profiler.contains(tag), "Missing tag: {}", tag);
    }

    // Verify process_tags
    assert_eq!(event_json["process_tags"], "entrypoint.name:main,pid:12345");

    // Verify attachments
    let attachments = event_json["attachments"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing attachments"))?;
    assert_eq!(attachments.len(), 3);
    for attachment in &["profile.pprof", "jit.pprof", "metadata.json"] {
        assert!(
            attachments.contains(&serde_json::json!(attachment)),
            "Missing attachment: {}",
            attachment
        );
    }

    // Verify internal metadata was merged
    let internal = event_json["internal"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Missing internal metadata"))?;
    assert_eq!(internal["no_signals_workaround_enabled"], "true");
    assert_eq!(internal["execution_trace_enabled"], "false");
    assert_eq!(internal["custom_field"], "custom_value");
    assert!(internal.contains_key("libdatadog_version"));

    // Verify info was included
    assert_eq!(event_json["info"]["application"]["env"], "production");
    assert_eq!(event_json["info"]["platform"]["hostname"], "test-host");
    assert_eq!(event_json["info"]["runtime"]["engine"], "rust");

    // Verify parts exist (files are compressed, just check non-empty)
    for part_name in &["profile.pprof", "jit.pprof", "metadata.json"] {
        let part = parts
            .iter()
            .find(|p| p.name == *part_name)
            .ok_or_else(|| anyhow::anyhow!("Missing part: {}", part_name))?;
        assert!(
            !part.content.is_empty(),
            "{} should not be empty",
            part_name
        );
    }

    // Verify all parts have correct MIME types
    common::assert_all_standard_mime_types(parts);

    Ok(())
}

/// Helper to test agent endpoint with a specific transport
async fn test_agent_with_transport(transport: Transport) -> anyhow::Result<()> {
    let server = spawn_server(transport).await?;

    // Configure agent endpoint based on transport
    let endpoint = if let Some(port) = server.port {
        let endpoint_url = format!("http://127.0.0.1:{}", port).parse()?;
        config::agent(endpoint_url)?
    } else {
        #[cfg(unix)]
        {
            let socket_path = server.socket_path.as_ref().unwrap();
            config::agent_uds(socket_path)?
        }
        #[cfg(windows)]
        {
            let pipe_path = server.pipe_path.as_ref().unwrap();
            config::agent_named_pipe(pipe_path)?
        }
        #[cfg(not(any(unix, windows)))]
        anyhow::bail!("No port, socket path, or pipe path available")
    };

    // Run the full export test
    let req =
        export_full_profile(endpoint, RequestSource::Captured(server.received_requests)).await?;

    // Validate
    validate_full_export(&req, "/profiling/v1/input")?;

    // Cleanup if needed
    #[cfg(unix)]
    if let Some(path) = server.socket_path {
        let _ = std::fs::remove_file(&path);
    }

    Ok(())
}

/// Helper to test agentless endpoint with a specific transport
async fn test_agentless_with_transport(transport: Transport) -> anyhow::Result<()> {
    let server = spawn_server(transport).await?;

    // Configure agentless endpoint based on transport
    let endpoint = if let Some(port) = server.port {
        let endpoint_url = format!("http://127.0.0.1:{}/api/v2/profile", port).parse()?;
        let mut endpoint = libdd_common::Endpoint::from_url(endpoint_url);
        endpoint.api_key = Some("test-api-key-12345".into());
        endpoint
    } else {
        #[cfg(unix)]
        {
            let socket_path = server.socket_path.as_ref().unwrap();
            // For Unix sockets, we need to create endpoint manually
            let endpoint_url = libdd_common::connector::uds::socket_path_to_uri(socket_path)?;
            let mut parts = endpoint_url.into_parts();
            parts.path_and_query = Some("/api/v2/profile".parse()?);
            let url = http::Uri::from_parts(parts)?;
            let mut endpoint = libdd_common::Endpoint::from_url(url);
            endpoint.api_key = Some("test-api-key-12345".into());
            endpoint
        }
        #[cfg(windows)]
        {
            let pipe_path = server.pipe_path.as_ref().unwrap();
            // For named pipes, we need to create endpoint manually
            let endpoint_url =
                libdd_common::connector::named_pipe::named_pipe_path_to_uri(pipe_path)?;
            let mut parts = endpoint_url.into_parts();
            parts.path_and_query = Some("/api/v2/profile".parse()?);
            let url = http::Uri::from_parts(parts)?;
            let mut endpoint = libdd_common::Endpoint::from_url(url);
            endpoint.api_key = Some("test-api-key-12345".into());
            endpoint
        }
        #[cfg(not(any(unix, windows)))]
        anyhow::bail!("No port, socket path, or pipe path available")
    };

    // Run the full export test
    let req =
        export_full_profile(endpoint, RequestSource::Captured(server.received_requests)).await?;

    // Validate - agentless uses /api/v2/profile path
    validate_full_export(&req, "/api/v2/profile")?;

    // Verify API key header is present
    assert!(
        req.headers.contains_key("dd-api-key"),
        "DD-API-KEY header should be present for agentless"
    );

    // Cleanup if needed
    #[cfg(unix)]
    if let Some(path) = server.socket_path {
        let _ = std::fs::remove_file(&path);
    }

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_export_agent_tcp() -> anyhow::Result<()> {
    test_agent_with_transport(Transport::Tcp).await
}

#[cfg(unix)]
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_export_agent_uds() -> anyhow::Result<()> {
    test_agent_with_transport(Transport::UnixSocket).await
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_export_agentless_tcp() -> anyhow::Result<()> {
    test_agentless_with_transport(Transport::Tcp).await
}

#[cfg(unix)]
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_export_agentless_uds() -> anyhow::Result<()> {
    test_agentless_with_transport(Transport::UnixSocket).await
}

#[cfg(windows)]
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_export_agent_named_pipe() -> anyhow::Result<()> {
    test_agent_with_transport(Transport::NamedPipe).await
}

#[cfg(windows)]
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_export_agentless_named_pipe() -> anyhow::Result<()> {
    test_agentless_with_transport(Transport::NamedPipe).await
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_export_file() -> anyhow::Result<()> {
    let file_path = create_temp_file_path("http");
    let endpoint = config::file(file_path.to_string_lossy().as_ref())?;

    // Test and capture
    let req = export_full_profile(endpoint, RequestSource::File(file_path.clone())).await?;

    // Validate
    validate_full_export(&req, "/")?;

    // Cleanup
    let _ = std::fs::remove_file(&file_path);

    Ok(())
}
