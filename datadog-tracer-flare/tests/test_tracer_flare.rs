// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_remote_config::config::agent_task::{AgentTask, AgentTaskFile};
use datadog_tracer_flare::{run_remote_config_listener, FlareAction, LogLevel, TracerFlareManager};
use http_body_util::BodyExt;
use hyper::service::service_fn;
use hyper::StatusCode;
use hyper_util::rt::tokio::TokioIo;
use libdd_common::hyper_migration;
use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
use std::convert::Infallible;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};

fn create_test_files(temp_dir: &TempDir) -> Vec<String> {
    // Minimal file set to exercise zip_and_send.
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "test content").unwrap();

    let dir = temp_dir.path().join("dir");
    std::fs::create_dir(&dir).unwrap();
    let sub_file = dir.join("subfile.txt");
    std::fs::write(&sub_file, "sub file content").unwrap();

    vec![
        file_path.to_string_lossy().into_owned(),
        dir.to_string_lossy().into_owned(),
    ]
}

async fn spawn_flare_receiver() -> (String, oneshot::Receiver<Vec<u8>>, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (body_tx, body_rx) = oneshot::channel::<Vec<u8>>();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let body_tx = Arc::new(Mutex::new(Some(body_tx)));

    let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let body_tx = body_tx.clone();
        async move {
            // Ensure the TracerFlareManager uses the expected endpoint.
            assert_eq!(req.uri().path(), "/tracer_flare/v1");
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            if let Some(tx) = body_tx.lock().await.take() {
                let _ = tx.send(body_bytes.to_vec());
            }
            let response = hyper::Response::builder()
                .status(StatusCode::OK)
                .body(hyper_migration::Body::from(""))
                .unwrap();
            Ok::<_, Infallible>(response)
        }
    });

    tokio::spawn(async move {
        loop {
            let stream = tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => accept.unwrap().0,
            };
            let service = service.clone();
            tokio::spawn(async move {
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                    .unwrap();
            });
        }
    });

    (format!("http://{addr}"), body_rx, shutdown_tx)
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn integration_listener_set_then_send() {
    // End-to-end Remote Config flow against the Datadog test agent.
    let test_agent = DatadogTestAgent::new(None, None, &[]).await;
    let mut manager = TracerFlareManager::new_with_listener(
        test_agent.get_base_uri().await.to_string(),
        "rust".to_string(),
        "1.0.0".to_string(),
        "test-service".to_string(),
        "test-env".to_string(),
        "1.0.0".to_string(),
        "test-runtime".to_string(),
    )
    .unwrap();

    let config_response = r#"{
        "path": "datadog/2/AGENT_CONFIG/rc-config-1/config",
        "msg": {
            "config": { "log_level": "debug" },
            "name": "flare-log-level.debug"
        }
    }"#;
    test_agent
        .set_remote_config_response(config_response, None)
        .await;

    let action = run_remote_config_listener(&mut manager).await.unwrap();
    assert!(matches!(action, FlareAction::Set(_)));

    let task_response = r#"{
        "path": "datadog/2/AGENT_TASK/rc-task-1/config",
        "msg": {
            "args": {
                "case_id": "12345",
                "hostname": "my-host-name",
                "user_handle": "my-user@datadoghq.com"
            },
            "task_type": "tracer_flare",
            "uuid": "550e8400-e29b-41d4-a716-446655440000"
        }
    }"#;
    test_agent
        .set_remote_config_response(task_response, None)
        .await;

    let action = run_remote_config_listener(&mut manager).await.unwrap();
    assert!(matches!(action, FlareAction::Send(_)));
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn integration_zip_and_send_payload_fields() {
    // End-to-end payload validation: multipart field order and expected content.
    let (agent_url, body_rx, shutdown_tx) = spawn_flare_receiver().await;
    let manager = TracerFlareManager::new(&agent_url, "rust");
    let temp_dir = TempDir::new().unwrap();
    let files = create_test_files(&temp_dir);
    let task = AgentTaskFile {
        args: AgentTask {
            case_id: "123456".to_string(),
            hostname: "myhostname".to_string(),
            user_handle: "user.name@datadoghq.com".to_string(),
        },
        task_type: "tracer_flare".to_string(),
        uuid: "d53fc8a4-8820-47a2-aa7d-d565582feb81".to_string(),
    };

    manager
        .zip_and_send(files, FlareAction::Send(task))
        .await
        .unwrap();

    let body = body_rx.await.unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(body_str.contains("d53fc8a4-8820-47a2-aa7d-d565582feb81"));
    assert!(!body_str.contains("DD-API-KEY"));
    assert!(!body_str.contains("dd-api-key"));

    let mut field_names = Vec::new();
    for line in body_str.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Content-Disposition: form-data; name=\"") {
            if let Some(end) = rest.find('"') {
                field_names.push(rest[..end].to_string());
            }
        }
    }

    assert_eq!(
        field_names,
        vec![
            "source",
            "case_id",
            "hostname",
            "email",
            "uuid",
            "flare_file",
        ]
    );

    let _ = shutdown_tx.send(());
}

fn build_agent_config_response(name_suffix: &str, log_level: &str) -> String {
    format!(
        r#"{{
            "path": "datadog/2/AGENT_CONFIG/rc-config-1/config",
            "msg": {{
                "config": {{ "log_level": "{log_level}" }},
                "name": "flare-log-level.{name_suffix}"
            }}
        }}"#
    )
}

fn build_agent_task_response(case_id: &str) -> String {
    format!(
        r#"{{
            "path": "datadog/2/AGENT_TASK/rc-task-1/config",
            "msg": {{
                "args": {{
                    "case_id": "{case_id}",
                    "hostname": "my-host-name",
                    "user_handle": "my-user@datadoghq.com"
                }},
                "task_type": "tracer_flare",
                "uuid": "550e8400-e29b-41d4-a716-446655440000"
            }}
        }}"#
    )
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn integration_listener_task_without_config() {
    let test_agent = DatadogTestAgent::new(None, None, &[]).await;
    let mut manager = TracerFlareManager::new_with_listener(
        test_agent.get_base_uri().await.to_string(),
        "rust".to_string(),
        "1.0.0".to_string(),
        "test-service".to_string(),
        "test-env".to_string(),
        "1.0.0".to_string(),
        "test-runtime".to_string(),
    )
    .unwrap();

    let task_response = build_agent_task_response("99999");
    test_agent
        .set_remote_config_response(&task_response, None)
        .await;

    let action = run_remote_config_listener(&mut manager).await.unwrap();
    assert!(matches!(action, FlareAction::Send(_)));
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn integration_listener_config_updates_log_level() {
    let test_agent = DatadogTestAgent::new(None, None, &[]).await;
    let mut manager = TracerFlareManager::new_with_listener(
        test_agent.get_base_uri().await.to_string(),
        "rust".to_string(),
        "1.0.0".to_string(),
        "test-service".to_string(),
        "test-env".to_string(),
        "1.0.0".to_string(),
        "test-runtime".to_string(),
    )
    .unwrap();

    assert!(!manager.is_collecting());
    // Start a flare with a debug log level
    let config_debug = build_agent_config_response("debug", "debug");
    test_agent
        .set_remote_config_response(&config_debug, None)
        .await;
    let action = run_remote_config_listener(&mut manager).await.unwrap();
    assert!(matches!(action, FlareAction::Set(LogLevel::Debug)));
    assert!(manager.is_collecting());

    // Start another flare with a warn log level
    let config_warn = build_agent_config_response("warn", "warn");
    test_agent
        .set_remote_config_response(&config_warn, None)
        .await;

    // The debug log level should be preserved since it has higher priority so the action should be None
    let action = run_remote_config_listener(&mut manager).await.unwrap();
    assert!(matches!(action, FlareAction::None));
    assert!(manager.is_collecting());

}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn integration_listener_ignores_non_flare_config() {
    let test_agent = DatadogTestAgent::new(None, None, &[]).await;
    let mut manager = TracerFlareManager::new_with_listener(
        test_agent.get_base_uri().await.to_string(),
        "rust".to_string(),
        "1.0.0".to_string(),
        "test-service".to_string(),
        "test-env".to_string(),
        "1.0.0".to_string(),
        "test-runtime".to_string(),
    )
    .unwrap();

    let config_response = r#"{
        "path": "datadog/2/AGENT_CONFIG/rc-config-1/config",
        "msg": {
            "config": { "log_level": "debug" },
            "name": "not-a-flare-config"
        }
    }"#;
    test_agent
        .set_remote_config_response(config_response, None)
        .await;

    let action = run_remote_config_listener(&mut manager).await.unwrap();
    assert!(matches!(action, FlareAction::None));
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn integration_listener_invalid_log_level_returns_error() {
    let test_agent = DatadogTestAgent::new(None, None, &[]).await;
    let mut manager = TracerFlareManager::new_with_listener(
        test_agent.get_base_uri().await.to_string(),
        "rust".to_string(),
        "1.0.0".to_string(),
        "test-service".to_string(),
        "test-env".to_string(),
        "1.0.0".to_string(),
        "test-runtime".to_string(),
    )
    .unwrap();

    let config_invalid = build_agent_config_response("invalid", "not-a-level");
    test_agent
        .set_remote_config_response(&config_invalid, None)
        .await;

    let result = run_remote_config_listener(&mut manager).await;
    assert!(result.is_err());
}
