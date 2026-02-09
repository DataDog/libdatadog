// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracing_integration_tests {
    use datadog_remote_config::config::agent_task::{AgentTask, AgentTaskFile};
    use datadog_tracer_flare::{
        run_remote_config_listener, FlareAction, LogLevel, TracerFlareManager,
    };
    use httpmock::prelude::{MockServer, POST};
    use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

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

    fn build_agent_config_response(log_level: &str) -> String {
        format!(
            r#"{{
                "path": "datadog/2/AGENT_CONFIG/rc-config-1/config",
                "msg": {{
                    "config": {{ "log_level": "{log_level}" }},
                    "name": "flare-log-level.{log_level}"
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
    async fn test_listener_set_then_send() {
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

        let config_response = build_agent_config_response("debug");
        test_agent
            .set_remote_config_response(config_response.as_str(), None)
            .await;

        let action = run_remote_config_listener(&mut manager).await.unwrap();
        assert!(matches!(action, FlareAction::Set(_)));

        let task_response = build_agent_task_response("12345");
        test_agent
            .set_remote_config_response(task_response.as_str(), None)
            .await;

        let action = run_remote_config_listener(&mut manager).await.unwrap();
        assert!(matches!(action, FlareAction::Send(_)));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_zip_and_send_payload_fields() {
        // End-to-end payload validation: multipart field order and expected content.
        let server = MockServer::start_async().await;
        let captured_body = Arc::new(Mutex::new(None));
        let captured_body_for_mock = captured_body.clone();
        let mock = server
            .mock_async(move |when, then| {
                let captured_body = captured_body_for_mock.clone();
                when.method(POST)
                    .path("/tracer_flare/v1")
                    .is_true(move |req| {
                        let mut guard = captured_body.lock().unwrap();
                        if guard.is_none() {
                            *guard = Some(req.body_vec());
                        }
                        true
                    });
                then.status(200);
            })
            .await;
        let agent_url = server.url("/");
        let agent_url = agent_url.trim_end_matches('/').to_string();
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

        mock.assert_async().await;
        let body = captured_body
            .lock()
            .unwrap()
            .clone()
            .expect("expected captured tracer flare request body");
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
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_listener_task_without_config() {
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
    async fn test_listener_config_updates_log_level() {
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
        let config_debug = build_agent_config_response("debug");
        test_agent
            .set_remote_config_response(&config_debug, None)
            .await;
        let action = run_remote_config_listener(&mut manager).await.unwrap();
        assert!(matches!(action, FlareAction::Set(LogLevel::Debug)));
        assert!(manager.is_collecting());

        // Start another flare with a warn log level
        let config_warn = build_agent_config_response("warn");
        test_agent
            .set_remote_config_response(&config_warn, None)
            .await;

        // The debug log level should be preserved since it has higher priority so the action should
        // be None
        let action = run_remote_config_listener(&mut manager).await.unwrap();
        assert!(matches!(action, FlareAction::None));
        assert!(manager.is_collecting());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_listener_ignores_non_flare_config() {
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
    async fn test_listener_invalid_log_level_returns_error() {
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

        let config_invalid = build_agent_config_response("invalid");
        test_agent
            .set_remote_config_response(&config_invalid, None)
            .await;

        let result = run_remote_config_listener(&mut manager).await;
        assert!(result.is_err());
    }
}
