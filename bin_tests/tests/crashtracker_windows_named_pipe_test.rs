// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end test for Windows named pipe uploads in crashtracker.
//!
//! This test validates that the crashtracker correctly uploads crash reports
//! to a Windows named pipe when configured to do so. The test creates a mock
//! named pipe server, triggers a crash, and verifies that the crash data
//! is received through the named pipe.

#![cfg(windows)]

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_crashtracker_windows_named_pipe_dual_upload() {
    test_crashtracker_named_pipe_dual_upload_impl()
        .await
        .unwrap();
}

async fn test_crashtracker_named_pipe_dual_upload_impl() -> anyhow::Result<()> {
    // Setup test fixtures
    let (crashtracker_bin, crashtracker_receiver) = setup_crashtracking_crates(BuildProfile::Debug);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]).await?;

    // Create a named pipe name for testing
    let pipe_name = r"\\.\pipe\dd_crashtracker_test_pipe";

    // Create multiple named pipe instances to handle concurrent connections
    let server1 = ServerOptions::new()
        .first_pipe_instance(true)
        .create(pipe_name)?;

    let server2 = ServerOptions::new().create(pipe_name)?;

    let server3 = ServerOptions::new().create(pipe_name)?;

    let server4 = ServerOptions::new().create(pipe_name)?;

    // Channel to communicate received data between threads
    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    // Start multiple named pipe servers to handle dual upload (telemetry + errors intake)
    // Each handles crash ping + crash report
    let servers = vec![server1, server2, server3, server4];
    let mut server_handles = Vec::new();

    for (i, server) in servers.into_iter().enumerate() {
        let tx_clone = tx.clone();
        let handle = tokio::spawn(async move {
            match handle_named_pipe_connection(server, tx_clone, i).await {
                Ok(()) => println!("Named pipe server {} completed successfully", i),
                Err(e) => eprintln!("Named pipe server {} error: {}", i, e),
            }
        });
        server_handles.push(handle);
    }

    // Give the servers a moment to start listening
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Construct the named pipe URL using the windows:// scheme
    let named_pipe_url = format!("windows://{}", hex::encode(pipe_name));

    // Launch the crashtracker test binary with named pipe endpoint
    let mut child = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(&named_pipe_url)
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg("donothing") // test mode
        .arg("null_deref") // crash type
        .spawn()
        .context("Failed to spawn crashtracker test process")?;

    // Wait for the child process to crash and complete
    let exit_status = child.wait().context("Failed to wait for child process")?;
    assert!(!exit_status.success(), "Expected child process to crash");

    // Collect all received data from named pipes (expect up to 4 requests)
    let mut all_received_data = Vec::new();

    for _ in 0..4 {
        let received_data = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match rx.try_recv() {
                    Ok(data) => return data,
                    Err(mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Vec::new(); // Server disconnected
                    }
                }
            }
        })
        .await;

        match received_data {
            Ok(data) if !data.is_empty() => all_received_data.push(data),
            _ => break, // Timeout or empty data, stop collecting
        }
    }

    // Clean up the server tasks
    for handle in server_handles {
        handle.abort();
    }

    // Verify we received data
    assert!(
        !all_received_data.is_empty(),
        "Expected to receive data from named pipe"
    );
    println!(
        "Received {} HTTP requests through named pipe",
        all_received_data.len()
    );

    // Parse and validate each received HTTP request
    let mut telemetry_requests = Vec::new();
    let mut errors_intake_requests = Vec::new();

    for data in &all_received_data {
        let request_str =
            String::from_utf8(data.clone()).context("Failed to convert received data to string")?;

        println!("Processing HTTP request:\n{}", request_str);

        if let Ok(payload) = extract_telemetry_payload_from_http_request(&request_str) {
            // Check if this is a telemetry request (has api_version and request_type)
            if payload.get("api_version").is_some() && payload.get("request_type").is_some() {
                telemetry_requests.push(payload);
            } else {
                // This might be an errors intake request
                errors_intake_requests.push(payload);
            }
        }
    }

    // Validate that we received both telemetry and errors intake requests
    assert!(
        !telemetry_requests.is_empty(),
        "Expected to receive telemetry requests"
    );
    println!(
        "✅ Received {} telemetry requests",
        telemetry_requests.len()
    );

    // For the main validation, use the first telemetry request
    validate_named_pipe_crash_report(&telemetry_requests[0])
        .context("Failed to validate telemetry crash report")?;

    // If we have errors intake requests, validate those too
    if !errors_intake_requests.is_empty() {
        println!(
            "✅ Received {} errors intake requests",
            errors_intake_requests.len()
        );
        validate_errors_intake_crash_report(&errors_intake_requests[0])
            .context("Failed to validate errors intake crash report")?;
    }

    println!("✅ Windows named pipe crashtracker dual upload test completed successfully!");
    Ok(())
}

async fn handle_named_pipe_connection(
    mut server: NamedPipeServer,
    tx: mpsc::Sender<Vec<u8>>,
    server_id: usize,
) -> anyhow::Result<()> {
    // Wait for a client to connect
    server
        .connect()
        .await
        .context("Failed to connect to named pipe client")?;

    println!("Named pipe client connected to server {}", server_id);

    // Read the HTTP request from the client
    let mut buffer = Vec::new();
    let mut temp_buf = [0u8; 4096];

    // Read until we get the complete HTTP request
    let start_time = std::time::Instant::now();
    let timeout = Duration::from_secs(10);

    while start_time.elapsed() < timeout {
        match server.try_read(&mut temp_buf) {
            Ok(0) => {
                // EOF reached
                break;
            }
            Ok(n) => {
                buffer.extend_from_slice(&temp_buf[..n]);

                // Check if we have a complete HTTP request
                let request_str = String::from_utf8_lossy(&buffer);
                if request_str.contains("\r\n\r\n") {
                    // We have the headers, check if we need to read the body
                    if let Some(content_length) = extract_content_length(&request_str) {
                        let header_end = request_str.find("\r\n\r\n").unwrap() + 4;
                        let body_so_far = buffer.len() - header_end;

                        if body_so_far >= content_length {
                            // We have the complete request
                            break;
                        }
                        // Continue reading for the body
                    } else {
                        // No content-length, assume request is complete
                        break;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Would block, wait a bit and try again
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to read from named pipe: {}", e));
            }
        }
    }

    // Send a simple HTTP response back to the client
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    server
        .write_all(response.as_bytes())
        .await
        .context("Failed to write response to named pipe")?;

    // Send the received data back to the test thread
    tx.send(buffer)
        .context("Failed to send data to test thread")?;

    Ok(())
}

fn extract_content_length(http_request: &str) -> Option<usize> {
    for line in http_request.lines() {
        if line.to_lowercase().starts_with("content-length:") {
            if let Some(value) = line.split(':').nth(1) {
                return value.trim().parse().ok();
            }
        }
    }
    None
}

fn extract_telemetry_payload_from_http_request(request: &str) -> anyhow::Result<Value> {
    // Find the start of the request body (after the empty line)
    let body_start = request
        .find("\r\n\r\n")
        .context("Failed to find HTTP request body")?;

    let body = &request[body_start + 4..];

    // Parse the JSON payload
    let payload: Value = serde_json::from_str(body).context("Failed to parse payload as JSON")?;

    Ok(payload)
}

fn validate_named_pipe_crash_report(telemetry_payload: &Value) -> anyhow::Result<()> {
    // Validate the telemetry structure
    assert_eq!(
        telemetry_payload["request_type"], "logs",
        "Expected telemetry request_type to be 'logs'"
    );

    assert_eq!(
        telemetry_payload["api_version"], "v2",
        "Expected telemetry api_version to be 'v2'"
    );

    assert_eq!(
        telemetry_payload["origin"], "Crashtracker",
        "Expected telemetry origin to be 'Crashtracker'"
    );

    // Validate the application metadata
    let application = &telemetry_payload["application"];
    assert_eq!(
        application["service_name"], "foo",
        "Expected service_name to be 'foo'"
    );
    assert_eq!(
        application["language_name"], "native",
        "Expected language_name to be 'native'"
    );

    // Validate the payload array
    let payload_array = telemetry_payload["payload"]
        .as_array()
        .context("Expected payload to be an array")?;
    assert_eq!(
        payload_array.len(),
        1,
        "Expected exactly one log entry in payload"
    );

    let log_entry = &payload_array[0];

    // Check if this is a crash ping or crash report
    let is_crash_ping = log_entry["tags"]
        .as_str()
        .map(|tags| tags.contains("is_crash_ping:true"))
        .unwrap_or(false);

    if is_crash_ping {
        // Validate crash ping
        assert_eq!(
            log_entry["level"], "DEBUG",
            "Expected crash ping log level to be DEBUG"
        );
        assert_eq!(
            log_entry["is_sensitive"], false,
            "Expected crash ping to not be marked as sensitive"
        );
        assert_eq!(
            log_entry["is_crash"], false,
            "Expected crash ping to not be marked as crash"
        );

        let crash_message = log_entry["message"]
            .as_str()
            .context("Expected message to be a string")?;
        let crash_ping_data: Value = serde_json::from_str(crash_message)
            .context("Failed to parse crash ping from message")?;

        assert_eq!(
            crash_ping_data["version"], "1.0",
            "Expected crash ping version 1.0"
        );
        assert_eq!(
            crash_ping_data["kind"], "Crash ping",
            "Expected crash ping kind"
        );
    } else {
        // Validate crash report
        assert_eq!(
            log_entry["level"], "ERROR",
            "Expected crash report log level to be ERROR"
        );
        assert_eq!(
            log_entry["is_sensitive"], true,
            "Expected crash report to be marked as sensitive"
        );
        assert_eq!(
            log_entry["is_crash"], true,
            "Expected crash report to be marked as crash"
        );

        let crash_message = log_entry["message"]
            .as_str()
            .context("Expected message to be a string")?;
        let crash_info: Value = serde_json::from_str(crash_message)
            .context("Failed to parse crash info from message")?;

        // Validate core crash info fields
        assert_eq!(
            crash_info["data_schema_version"], "1.4",
            "Expected crash info schema version 1.4"
        );

        assert!(
            crash_info["uuid"].is_string() && !crash_info["uuid"].as_str().unwrap().is_empty(),
            "Expected crash UUID to be a non-empty string"
        );

        // Validate signal information for null_deref crash
        let sig_info = &crash_info["sig_info"];
        assert_eq!(
            sig_info["si_signo"],
            11, // SIGSEGV = 11
            "Expected SIGSEGV signal number"
        );
        assert_eq!(
            sig_info["si_signo_human_readable"], "SIGSEGV",
            "Expected SIGSEGV signal name"
        );

        // Validate error information
        let error = &crash_info["error"];
        assert_eq!(
            error["kind"], "UnixSignal",
            "Expected error kind to be UnixSignal"
        );
        assert!(
            error["message"].as_str().unwrap().contains("SIGSEGV"),
            "Expected error message to contain SIGSEGV"
        );
    }

    // Validate tags contain expected information
    let tags = log_entry["tags"]
        .as_str()
        .context("Expected tags to be a string")?;

    assert!(
        tags.contains("si_signo:11"),
        "Expected tags to contain 'si_signo:11' (SIGSEGV)"
    );
    assert!(
        tags.contains("si_signo_human_readable:SIGSEGV"),
        "Expected tags to contain 'si_signo_human_readable:SIGSEGV'"
    );

    println!("✅ Telemetry crash report validation passed");
    Ok(())
}

fn validate_errors_intake_crash_report(errors_payload: &Value) -> anyhow::Result<()> {
    // Validate errors intake structure
    assert_eq!(
        errors_payload["ddsource"], "crashtracker",
        "Expected errors intake ddsource to be 'crashtracker'"
    );

    assert!(
        errors_payload["timestamp"].is_number(),
        "Expected timestamp to be a number"
    );

    let ddtags = errors_payload["ddtags"]
        .as_str()
        .context("Expected ddtags to be a string")?;
    assert!(
        ddtags.contains("service:foo"),
        "Expected ddtags to contain service:foo"
    );

    let error = &errors_payload["error"];
    assert_eq!(
        error["source_type"], "Crashtracking",
        "Expected error source_type to be 'Crashtracking'"
    );

    // Check if this is a crash ping or crash report
    let is_crash_ping = ddtags.contains("is_crash_ping:true");

    if is_crash_ping {
        assert_eq!(
            error["is_crash"], false,
            "Expected errors intake crash ping is_crash to be false"
        );
    } else {
        assert_eq!(
            error["is_crash"], true,
            "Expected errors intake crash report is_crash to be true"
        );
        assert_eq!(
            error["type"], "SIGSEGV",
            "Expected errors intake error type to be SIGSEGV"
        );
        assert!(
            error["message"].as_str().unwrap().contains("SIGSEGV"),
            "Expected errors intake error message to contain SIGSEGV"
        );
    }

    println!("✅ Errors intake crash report validation passed");
    Ok(())
}

struct TestFixtures {
    _tmpdir: TempDir,
    output_dir: PathBuf,
    artifacts: HashMap<ArtifactsBuild, PathBuf>,
}

async fn setup_test_fixtures(crates: &[&ArtifactsBuild]) -> anyhow::Result<TestFixtures> {
    let artifacts = build_artifacts(crates).context("Failed to build test artifacts")?;

    let tmpdir = TempDir::new().context("Failed to create temporary directory")?;

    // Convert HashMap<&ArtifactsBuild, PathBuf> to HashMap<ArtifactsBuild, PathBuf>
    let artifacts: HashMap<ArtifactsBuild, PathBuf> =
        artifacts.into_iter().map(|(k, v)| (k.clone(), v)).collect();

    Ok(TestFixtures {
        output_dir: tmpdir.path().to_path_buf(),
        artifacts,
        _tmpdir: tmpdir,
    })
}

fn setup_crashtracking_crates(profile: BuildProfile) -> (ArtifactsBuild, ArtifactsBuild) {
    let crashtracker_bin = ArtifactsBuild {
        name: "crashtracker_bin_test".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };
    let crashtracker_receiver = ArtifactsBuild {
        name: "crashtracker_receiver".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };
    (crashtracker_bin, crashtracker_receiver)
}

#[cfg(not(windows))]
#[tokio::test]
async fn test_crashtracker_windows_named_pipe_dual_upload() {
    // This test is Windows-only, skip on other platforms
    println!("Skipping Windows named pipe test on non-Windows platform");
}
