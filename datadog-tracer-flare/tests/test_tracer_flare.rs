// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracer_flare_integration_tests {
    use std::{fs::read, str::FromStr};

    use datadog_remote_config::config;
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use datadog_tracer_flare::{
        run_remote_config_listener, LogLevel, ReturnAction, State, TracerFlareManager,
        
    };
    use ddcommon::hyper_migration::{self, Body};
    use hyper::Request;

    #[tokio::test]
    async fn listener_test() {
        let test_agent = DatadogTestAgent::new(None, None, &[]).await;
        let agent_url = test_agent.get_base_uri().await.to_string();
        // let agent_url = "http://0.0.0.0:8126/".to_string();

        let client = hyper_migration::new_default_client();

        // Start the TracerFlareManager
        let mut manager = TracerFlareManager::new_with_listener(
            agent_url.clone(),
            "rust".to_string(),
            "1.0.0".to_string(),
            "test-service".to_string(),
            "test-env".to_string(),
            "1.0.0".to_string(),
            "test-runtime".to_string(),
        )
        .unwrap();

        let config_uri = test_agent
            .get_uri_for_endpoint("test/session/responses/config/path", None)
            .await;
        // let mut config_uri = agent_url;
        // config_uri.push_str("test/session/responses/config/path");
        // let config_uri = hyper::Uri::from_str(&config_uri).unwrap();

        // Create an AGENT_CONFIG product
        let req_config = Request::builder()
            .method("POST")
            .uri(&config_uri)
            .body(Body::from(
                "{
                    \"path\": \"datadog/2/AGENT_CONFIG/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\",
                    \"msg\": {
                        \"config\": {
                            \"log_level\": \"debug\"
                        },
                        \"name\": \"flare-log-level.debug\"
                    }
                }",
            ))
            .expect("Failed to create AGENT_CONFIG request");

        let res = client.request(req_config).await;
        assert!(res.is_ok());
        println!("Res config : {res:?}");

        let action = run_remote_config_listener(&mut manager).await;
        println!("Action : {action:?}");

        // Create an AGENT_TASK product
        let req_task = Request::builder()
            .method("POST")
            .uri(&config_uri)
            .body(Body::from(
                "{
                    \"path\": \"datadog/2/AGENT_TASK/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\",
                    \"msg\": {
                        \"args\": {
                            \"case_id\": \"12345\",
                            \"hostname\": \"my-host-name\",
                            \"user_handle\": \"my-user@datadoghq.com\"
                        },
                        \"task_type\": \"tracer_flare\",
                        \"uuid\": \"550e8400-e29b-41d4-a716-446655440000\"
                    }
                }",
            ))
            .expect("Failed to create AGENT_TASK request");

        let res = client.request(req_task).await;
        assert!(res.is_ok());
        println!("Res task : {res:?}");

        let action = run_remote_config_listener(&mut manager).await;
        println!("Action : {action:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn receive_agent_config_test() {
        let test_agent = DatadogTestAgent::new(None, None, &[]).await;

         // Start the TracerFlareManager
        let mut manager = TracerFlareManager::new_with_listener(
            test_agent.get_base_uri().await.to_string(),
            "rust".to_string(),
            "1.0.0".to_string(),
            "test-service".to_string(),
            "test-env".to_string(),
            "1.0.0".to_string(),
            "test-runtime".to_string(),
        ).unwrap();

        let config_uri = test_agent
        .get_uri_for_endpoint("test/session/responses/config", None)
        .await;
        // Create an AGENT_CONFIG request
        let req_config = Request::builder()
            .method("POST")
            .uri(&config_uri)
            // The AGENT_CONFIG product is encoded into base64 in the `raw` field
            // {
            //     "config":{
            //         "log_level":"debug"
            //     },
            //     "name":"flare-log-level.debug"
            // }
            .body(Body::from("{
                \"roots\": [],
                \"targets\": \"ewogICAgInNpZ25lZCI6IHsKICAgICAgICAidGFyZ2V0cyI6IHsKICAgICAgICAgICAgImRhdGFkb2cvMi9BR0VOVF9DT05GSUcvZGFkNDU4NjUtMjVjYi00YjVlLTllMmQtODcxZmZiZmIwNDljL2NvbmZpZyI6IHsKICAgICAgICAgICAgICAgICJoYXNoZXMiOiB7CiAgICAgICAgICAgICAgICAgICAgInNoYTI1NiI6ICJiZTE4YTg4ODRlM2M5YWFiNjJkY2QxNTNjYjFjYTkxNzg4MTY5MDQ0ZmQzN2I2OTZjZDA5YmRkYWRmZDI0YTQ3IgogICAgICAgICAgICAgICAgfSwKICAgICAgICAgICAgICAgICJsZW5ndGgiOiAxMjMsCiAgICAgICAgICAgICAgICAiY3VzdG9tIjogewogICAgICAgICAgICAgICAgICAgICJ2IjogMQogICAgICAgICAgICAgICAgfQogICAgICAgICAgICB9CiAgICAgICAgfSwKICAgICAgICAidmVyc2lvbiI6IDEsCiAgICAgICAgImN1c3RvbSI6IHsKICAgICAgICAgICAgIm9wYXF1ZV9iYWNrZW5kX3N0YXRlIjogInN0YXRlX2RhdGEiLAogICAgICAgICAgICAiYWdlbnRfcmVmcmVzaF9pbnRlcnZhbCI6IDAKICAgICAgICB9LAogICAgICAgICJfdHlwZSI6ICJ0ZXN0X3R5cGUiLAogICAgICAgICJleHBpcmVzIjogIjIwMjUtMDUtMDVUMTQ6MzA6MDBaIiwKICAgICAgICAic3BlY192ZXJzaW9uIjogInRlc3RfdmVyc2lvbiIKICAgIH0sCiAgICAic2lnbmF0dXJlcyI6IFtdCn0=\",
                \"target_files\": [
                    {
                        \"path\":\"datadog/2/AGENT_CONFIG/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\",
                        \"raw\": \"ewogICAiY29uZmlnIjp7CiAgICAgICJsb2dfbGV2ZWwiOiJkZWJ1ZyIKICAgfSwKICAgIm5hbWUiOiJmbGFyZS1sb2ctbGV2ZWwuZGVidWciCn0=\"
                    }
                ],
                \"client_configs\": [
                    \"datadog/2/AGENT_CONFIG/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\"
                ]
            }"))
            .expect("Failed to create AGENT_CONFIG request");

        // Send it through the test agent
        let client = hyper_migration::new_default_client();
        let res = client.request(req_config).await;
        assert!(res.is_ok());
        println!("Res : {res:?}");

        // Create an AGENT_TASK request
        let req_task = Request::builder()
            .method("POST")
            .uri(&config_uri)
            // The AGENT_TASK product is encoded into base64 in the `raw` field
            // {
            //     "args": {
            //         "case_id": "12345",
            //         "hostname": "my-host-name",
            //         "user_handle": "my-user@datadoghq.com"
            //     },
            //     "task_type": "tracer_flare",
            //     "uuid": "550e8400-e29b-41d4-a716-446655440000"
            // }
            .body(Body::from("{
                \"roots\": [],
                \"targets\": \"ewogICAgInNpZ25lZCI6IHsKICAgICAgICAidGFyZ2V0cyI6IHsKICAgICAgICAgICAgImRhdGFkb2cvMi9BR0VOVF9UQVNLL2RhZDQ1ODY1LTI1Y2ItNGI1ZS05ZTJkLTg3MWZmYmZiMDQ5Yy9jb25maWciOiB7CiAgICAgICAgICAgICAgICAiaGFzaGVzIjogewogICAgICAgICAgICAgICAgICAgICJzaGEyNTYiOiAiMzA3MzFkNDcxY2FhN2I5ZjMyMDk1MTdlNTUyMGRiM2JmODliMmRiNTgyYjE4OWJjMzdhOGI1YjcyZGNlZThhYSIKICAgICAgICAgICAgICAgIH0sCiAgICAgICAgICAgICAgICAibGVuZ3RoIjogMTIzLAogICAgICAgICAgICAgICAgImN1c3RvbSI6IHsKICAgICAgICAgICAgICAgICAgICAidiI6IDEKICAgICAgICAgICAgICAgIH0KICAgICAgICAgICAgfQogICAgICAgIH0sCiAgICAgICAgInZlcnNpb24iOiAxLAogICAgICAgICJjdXN0b20iOiB7CiAgICAgICAgICAgICJvcGFxdWVfYmFja2VuZF9zdGF0ZSI6ICJzdGF0ZV9kYXRhIiwKICAgICAgICAgICAgImFnZW50X3JlZnJlc2hfaW50ZXJ2YWwiOiAwCiAgICAgICAgfSwKICAgICAgICAiX3R5cGUiOiAidGVzdF90eXBlIiwKICAgICAgICAiZXhwaXJlcyI6ICIyMDI1LTA1LTA1VDE0OjMwOjAwWiIsCiAgICAgICAgInNwZWNfdmVyc2lvbiI6ICJ0ZXN0X3ZlcnNpb24iCiAgICB9LAogICAgInNpZ25hdHVyZXMiOiBbXQp9Cg==\",
                \"target_files\": [
                    {
                        \"path\":\"datadog/2/AGENT_TASK/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\",
                        \"raw\": \"ewogICAgICJhcmdzIjogewogICAgICAgICAiY2FzZV9pZCI6ICIxMjM0NSIsCiAgICAgICAgICJob3N0bmFtZSI6ICJteS1ob3N0LW5hbWUiLAogICAgICAgICAidXNlcl9oYW5kbGUiOiAibXktdXNlckBkYXRhZG9naHEuY29tIgogICAgIH0sCiAgICAgInRhc2tfdHlwZSI6ICJ0cmFjZXJfZmxhcmUiLAogICAgICJ1dWlkIjogIjU1MGU4NDAwLWUyOWItNDFkNC1hNzE2LTQ0NjY1NTQ0MDAwMCIKfQ==\"
                    }
                ],
                \"client_configs\": [
                    \"datadog/2/AGENT_TASK/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\"
                ]
            }"))
            .expect("Failed to create AGENT_TASK request");

        // Send it through the test agent
        // let res = client.request(req_task).await;
        // assert!(res.is_ok());
        // println!("Res : {res:?}");

        // Listen one time
        // let action = run_remote_config_listener(&mut manager).await;

        // println!("Action : {action:?}");

        // Create a request with both AGENT_TASK and AGENT_CONFIG
        let req_both = Request::builder()
            .method("POST")
            .uri(&config_uri)
            .body(Body::from("{
                \"roots\": [],
                \"targets\": \"ewogICAgInNpZ25lZCI6IHsKICAgICAgICAidGFyZ2V0cyI6IHsKICAgICAgICAgICAgImRhdGFkb2cvMi9BR0VOVF9UQVNLL2RhZDQ1ODY1LTI1Y2ItNGI1ZS05ZTJkLTg3MWZmYmZiMDQ5Yy9jb25maWciOiB7CiAgICAgICAgICAgICAgICAiaGFzaGVzIjogewogICAgICAgICAgICAgICAgICAgICJzaGEyNTYiOiAiMzA3MzFkNDcxY2FhN2I5ZjMyMDk1MTdlNTUyMGRiM2JmODliMmRiNTgyYjE4OWJjMzdhOGI1YjcyZGNlZThhYSIKICAgICAgICAgICAgICAgIH0sCiAgICAgICAgICAgICAgICAibGVuZ3RoIjogMTIzLAogICAgICAgICAgICAgICAgImN1c3RvbSI6IHsKICAgICAgICAgICAgICAgICAgICAidiI6IDEKICAgICAgICAgICAgICAgIH0KICAgICAgICAgICAgfSwKICAgICAgICAgICAgImRhdGFkb2cvMi9BR0VOVF9DT05GSUcvZGFkNDU4NjUtMjVjYi00YjVlLTllMmQtODcxZmZiZmIwNDljL2NvbmZpZyI6IHsKICAgICAgICAgICAgICAgICJoYXNoZXMiOiB7CiAgICAgICAgICAgICAgICAgICAgInNoYTI1NiI6ICJiZTE4YTg4ODRlM2M5YWFiNjJkY2QxNTNjYjFjYTkxNzg4MTY5MDQ0ZmQzN2I2OTZjZDA5YmRkYWRmZDI0YTQ3IgogICAgICAgICAgICAgICAgfSwKICAgICAgICAgICAgICAgICJsZW5ndGgiOiAxMjMsCiAgICAgICAgICAgICAgICAiY3VzdG9tIjogewogICAgICAgICAgICAgICAgICAgICJ2IjogMQogICAgICAgICAgICAgICAgfQogICAgICAgICAgICB9CgogICAgICAgIH0sCiAgICAgICAgInZlcnNpb24iOiAxLAogICAgICAgICJjdXN0b20iOiB7CiAgICAgICAgICAgICJvcGFxdWVfYmFja2VuZF9zdGF0ZSI6ICJzdGF0ZV9kYXRhIiwKICAgICAgICAgICAgImFnZW50X3JlZnJlc2hfaW50ZXJ2YWwiOiAwCiAgICAgICAgfSwKICAgICAgICAiX3R5cGUiOiAidGVzdF90eXBlIiwKICAgICAgICAiZXhwaXJlcyI6ICIyMDI1LTA1LTA1VDE0OjMwOjAwWiIsCiAgICAgICAgInNwZWNfdmVyc2lvbiI6ICJ0ZXN0X3ZlcnNpb24iCiAgICB9LAogICAgInNpZ25hdHVyZXMiOiBbXQp9Cg==\",
                \"target_files\": [
                    {
                        \"path\":\"datadog/2/AGENT_CONFIG/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\",
                        \"raw\": \"ewogICAiY29uZmlnIjp7CiAgICAgICJsb2dfbGV2ZWwiOiJkZWJ1ZyIKICAgfSwKICAgIm5hbWUiOiJmbGFyZS1sb2ctbGV2ZWwuZGVidWciCn0=\"
                    },
                    {
                        \"path\":\"datadog/2/AGENT_TASK/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\",
                        \"raw\": \"ewogICAgICJhcmdzIjogewogICAgICAgICAiY2FzZV9pZCI6ICIxMjM0NSIsCiAgICAgICAgICJob3N0bmFtZSI6ICJteS1ob3N0LW5hbWUiLAogICAgICAgICAidXNlcl9oYW5kbGUiOiAibXktdXNlckBkYXRhZG9naHEuY29tIgogICAgIH0sCiAgICAgInRhc2tfdHlwZSI6ICJ0cmFjZXJfZmxhcmUiLAogICAgICJ1dWlkIjogIjU1MGU4NDAwLWUyOWItNDFkNC1hNzE2LTQ0NjY1NTQ0MDAwMCIKfQ==\"
                    }
                ],
                \"client_configs\": [
                    \"datadog/2/AGENT_CONFIG/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\",
                    \"datadog/2/AGENT_TASK/dad45865-25cb-4b5e-9e2d-871ffbfb049c/config\"
                ]
            }"))
            .expect("Failed to create AGENT_TASK and AGENT_CONFIG request");

        // Send it through the test agent
        let res = client.request(req_both).await;
        assert!(res.is_ok());
        println!("Res : {res:?}");

        // Listen one time
        let actions = run_remote_config_listener(&mut manager).await;

        println!("Actions : {actions:?}");
        assert!(actions.is_ok());
        // let action = action.unwrap();
        // assert_eq!(action, ReturnAction::Start(LogLevel::Debug));
        // assert_eq!(manager.state, State::Collecting { log_level: "debug".to_string() });
    }

    // use std::time::Duration;
    // use datadog_tracer_flare::{run_remote_config_listener, TracerFlareManager};
    // use tokio::time::sleep;

    // #[cfg_attr(miri, ignore)]
    // #[tokio::test]
    // async fn test_remote_config_listener() {
    //     // Test parameters
    //     let agent_url = "http://0.0.0.0:8126".to_string();
    //     let language = "rust".to_string();
    //     let tracer_version = "1.0.0".to_string();
    //     let service = "test-service".to_string();
    //     let env = "test-env".to_string();
    //     let app_version = "1.0.0".to_string();
    //     let runtime_id = "test-runtime".to_string();

    //     // Setup the listener
    //     #[allow(clippy::unwrap_used)]
    //     let mut manager = TracerFlareManager::new_with_listener(
    //         agent_url,
    //         language,
    //         tracer_version,
    //         service,
    //         env,
    //         app_version,
    //         runtime_id,
    //     )
    //     .unwrap();

    //     for _ in 0..10 {
    //         let result = run_remote_config_listener(&mut manager).await;
    //         println!("Result: {:?}", result);
    //         assert!(result.is_ok());
    //         sleep(Duration::from_secs(2)).await;
    //     }
    // }
}

// #[tokio::test]
// async fn test_send_and_zip() {
//     let agent_url = "http://0.0.0.0:8126".to_string();
//     let temp_dir = TempDir::new().unwrap();
//     let files = create_test_files(&temp_dir);

//     // Add a delay to ensure files are ready
//     // tokio::time::sleep(std::time::Duration::from_secs(15)).await;

//     let result = zip_and_send(files, agent_url).await;

//     // tokio::time::sleep(std::time::Duration::from_secs(15)).await;

//     assert!(result.is_ok())
// }
