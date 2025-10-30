// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use cargo_metadata::MetadataCommand;
use ddcommon::hyper_migration::{self, Body};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, Uri};
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

const TEST_AGENT_IMAGE_NAME: &str = "ghcr.io/datadog/dd-apm-test-agent/ddapm-test-agent";
const TEST_AGENT_IMAGE_TAG: &str = "v1.31.1";
const TEST_AGENT_READY_MSG: &str =
    "INFO:ddapm_test_agent.agent:Trace request stall seconds setting set to 0.0.";

const TEST_AGENT_PORT: u16 = 8126;
const SAMPLE_RATE_QUERY_PARAM_KEY: &str = "agent_sample_rate_by_service";
const SESSION_TEST_TOKEN_QUERY_PARAM_KEY: &str = "test_session_token";
const SESSION_START_ENDPOINT: &str = "test/session/start";
const SET_REMOTE_CONFIG_RESPONSE_PATH_ENDPOINT: &str = "test/session/responses/config/path";

struct DatadogAgentContainerBuilder {
    mounts: Vec<(String, String)>,
    env_vars: Vec<(String, String)>,
    exposed_port: u16,
}

struct DatadogTestAgentContainer {
    container_id: String,
    container_port: u16,
}

/// Run the command passed and returns an error if the return code is not
/// a success
fn run_command(c: &mut std::process::Command) -> anyhow::Result<std::process::Output> {
    let output = c.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "Running command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }
    Ok(output)
}

impl DatadogTestAgentContainer {
    fn stderr(&self) -> anyhow::Result<String> {
        use std::process::*;
        let output = run_command(Command::new("docker").arg("logs").arg(&self.container_id))
            .context("docker logs")?;
        Ok(String::from_utf8(output.stderr)?)
    }

    fn wait_ready(&self) -> anyhow::Result<()> {
        for _ in 0..100 {
            if self
                .stderr()
                .context("reading container logs")?
                .contains(TEST_AGENT_READY_MSG)
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        anyhow::bail!("waiting for test container timed out")
    }

    fn host_port(&self) -> anyhow::Result<String> {
        use std::process::*;
        let output = run_command(
            Command::new("docker")
                .args(["inspect", "--format"])
                .arg(format!(
                    r##"{{{{(index (index .NetworkSettings.Ports  "{}/tcp") 0).HostPort}}}}"##,
                    self.container_port
                ))
                .arg(&self.container_id),
        )
        .context("docker inspect mapped host port")?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn base_uri(&self) -> anyhow::Result<String> {
        Ok(format!("http://localhost:{}", self.host_port()?))
    }
}

impl Drop for DatadogTestAgentContainer {
    fn drop(&mut self) {
        use std::process::*;
        if let Err(e) = (|| -> anyhow::Result<()> {
            run_command(Command::new("docker").arg("stop").arg(&self.container_id))
                .context("docker stop container")?;
            Ok(())
        })() {
            eprintln!("error stopping test agent container: {e}");
        };
    }
}

impl DatadogAgentContainerBuilder {
    fn start(&self) -> anyhow::Result<DatadogTestAgentContainer> {
        use std::process::*;
        let mounts = self.mounts.iter().flat_map(|(host_path, container_path)| {
            ["-v".to_owned(), format!("{host_path}:{container_path}")]
        });
        let envs = self
            .env_vars
            .iter()
            .flat_map(|(e, v)| ["-e".to_owned(), format!("{e}={v}")]);

        let output = run_command(
            Command::new("docker")
                .args(["run", "--rm", "-d"])
                .args(mounts)
                .args(envs)
                .args(["-p".to_owned(), format!("{}", self.exposed_port)])
                .arg(format!("{TEST_AGENT_IMAGE_NAME}:{TEST_AGENT_IMAGE_TAG}",)),
        )
        .context("docker run container")?;
        let container_id = String::from_utf8(output.stdout)?.trim().to_owned();
        let container = DatadogTestAgentContainer {
            container_id,
            container_port: self.exposed_port,
        };
        container.wait_ready()?;
        Ok(container)
    }

    fn new(relative_snapshot_path: Option<&str>, absolute_socket_path: Option<&str>) -> Self {
        let mut env_vars = Vec::new();
        let mut mounts = Vec::new();

        if let Some(absolute_socket_path) = absolute_socket_path {
            env_vars.push((
                "DD_APM_RECEIVER_SOCKET".to_string(),
                "/tmp/ddsockets/apm.socket".to_owned(),
            ));

            mounts.push((absolute_socket_path.to_owned(), "/tmp/ddsockets".to_owned()));
        }

        if let Some(relative_snapshot_path) = relative_snapshot_path {
            mounts.push((
                Self::calculate_volume_absolute_path(relative_snapshot_path),
                "/snapshots".to_owned(),
            ));
        }

        DatadogAgentContainerBuilder {
            mounts,
            env_vars,
            exposed_port: TEST_AGENT_PORT,
        }
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env_vars.push((key.to_string(), value.to_string()));
        self
    }

    // The docker image requires an absolute path when mounting a volume. This function gets the
    // absolute path of the workspace and appends the provided relative path.
    fn calculate_volume_absolute_path(relative_snapshot_path: &str) -> String {
        let metadata = MetadataCommand::new()
            .exec()
            .expect("Failed to fetch metadata");

        let project_root_dir = metadata.workspace_root;

        let calculated_path = Path::new(&project_root_dir)
            .join(relative_snapshot_path)
            .as_os_str()
            .to_str()
            .expect("unable to convert OS string")
            .to_owned();

        calculated_path
    }
}

/// `DatadogTestAgent` is a wrapper around a containerized test agent that lives only for the test
/// it runs in. It has convenience functions to provide agent URIs, mount snapshot directories, and
/// assert  snapshot tests.
///
/// # Examples
///
/// Basic usage:
///
/// ```no_run
/// use datadog_trace_utils::send_data::SendData;
/// use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
/// use datadog_trace_utils::trace_utils::TracerHeaderTags;
/// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
/// use ddcommon::hyper_migration::new_default_client;
/// use ddcommon::Endpoint;
/// use libdd_trace_utils::send_data::SendData;
/// use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
/// use libdd_trace_utils::trace_utils::TracerHeaderTags;
/// use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
///
/// use tokio;
///
/// #[tokio::main]
/// async fn main() {
///     // Create a new DatadogTestAgent instance
///     let test_agent = DatadogTestAgent::new(Some("relative/path/to/snapshot"), None, &[]).await;
///
///     // Get the URI for a specific endpoint
///     let uri = test_agent
///         .get_uri_for_endpoint("test-endpoint", Some("snapshot-token"))
///         .await;
///
///     let endpoint = Endpoint::from_url(uri);
///
///     let trace = vec![];
///
///     let data = SendData::new(
///         100,
///         TracerPayloadCollection::V04(vec![trace.clone()]),
///         TracerHeaderTags::default(),
///         &endpoint,
///     );
///
///     let client = new_default_client();
///     let _result = data.send(&client).await;
///
///     // Assert that the snapshot for a given token matches the expected snapshot
///     test_agent.assert_snapshot("snapshot-token").await;
/// }
/// ```
pub struct DatadogTestAgent {
    container: DatadogTestAgentContainer,
}

impl DatadogTestAgent {
    /// Creates a new instance of `DatadogTestAgent` and starts a docker container hosting the
    /// test-agent. When `DatadogTestAgent` is dropped, the container will be stopped automatically.
    ///
    /// # Arguments
    ///
    /// * `relative_snapshot_path` - An optional string slice that holds the relative path to the
    ///   snapshot directory. This directory will get mounted in the docker container running the
    ///   test-agent. The relative path should include the crate name. If no relative path is
    ///   provided, no snapshot directory will be mounted.
    ///
    /// * `absolute_socket_path` - An optional string slice that holds the absolute path to the
    ///   socket directory. This directory will get mounted in the docker container running the
    ///   test-agent. It is recommended to use a temporary directory for this purpose. If no socket
    ///   path is provided the test agent will not be configured for UDS transport.
    /// * `test_agent_extra_env` - An optional slice of tuples containing env variables to be added
    ///   to the test agent container.
    /// # Returns
    ///
    /// A new `DatadogTestAgent`.
    pub async fn new(
        relative_snapshot_path: Option<&str>,
        absolute_socket_path: Option<&str>,
        test_agent_extra_env: &[(&str, &str)],
    ) -> Self {
        let mut container =
            DatadogAgentContainerBuilder::new(relative_snapshot_path, absolute_socket_path);
        for (key, value) in test_agent_extra_env {
            container = container.with_env(key, value);
        }

        DatadogTestAgent {
            container: container
                .start()
                .expect("Unable to start DatadogTestAgent, is the Docker Daemon running?"),
        }
    }

    async fn get_base_uri_string(&self) -> String {
        self.container.base_uri().unwrap()
    }

    /// Constructs the URI for a provided endpoint of the Datadog Test Agent by concatenating the
    /// host and port of the running test-agent and the endpoint. This is necessary because the
    /// docker-image dynamically assigns what port the test-agent's 8126 port is forwarded to.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - A string slice that holds the endpoint.
    /// * `snapshot_token` - An optional string slice that holds the snapshot token. If provided,
    ///   the token will be appended to the URI as a query parameter. This is necessary
    ///
    /// # Returns
    ///
    /// A `Uri` object representing the URI of the specified endpoint.
    pub async fn get_uri_for_endpoint(&self, endpoint: &str, snapshot_token: Option<&str>) -> Uri {
        let base_uri_string = self.get_base_uri_string().await;
        let uri_string = match snapshot_token {
            Some(token) => format!("{base_uri_string}/{endpoint}?test_session_token={token}"),
            None => format!("{base_uri_string}/{endpoint}"),
        };

        Uri::from_str(&uri_string).expect("Invalid URI")
    }

    async fn get_uri_for_endpoint_and_params(
        &self,
        endpoint: &str,
        query_params: HashMap<&str, &str>,
    ) -> Uri {
        let base_uri = self.get_base_uri().await;
        let mut parts = base_uri.into_parts();

        let query_string = if !query_params.is_empty() {
            let query = query_params
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            format!("?{query}")
        } else {
            String::new()
        };

        parts.path_and_query = Some(
            format!("/{}{}", endpoint.trim_start_matches('/'), query_string)
                .parse()
                .expect("Invalid path and query"),
        );

        Uri::from_parts(parts).expect("Invalid URI")
    }

    /// Returns the URI for the Datadog Test Agent's base URL and port.
    /// The docker-image dynamically assigns what port the test-agent's 8126 port is forwarded to.
    ///
    /// # Returns
    ///
    /// A `Uri` object representing the URI of the specified endpoint.
    pub async fn get_base_uri(&self) -> Uri {
        let base_uri_string = self.get_base_uri_string().await;
        Uri::from_str(&base_uri_string).expect("Invalid URI")
    }

    /// Asserts that the snapshot for a given token matches the expected snapshot. This should be
    /// called after sending data to the test-agent with the same token.
    ///
    /// # Arguments
    ///
    /// * `snapshot_token` - A string slice that holds the snapshot token.
    pub async fn assert_snapshot(&self, snapshot_token: &str) {
        let uri = self
            .get_uri_for_endpoint("test/session/snapshot", Some(snapshot_token))
            .await;

        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("Failed to create request");

        let res = self
            .agent_request_with_retry(req, 5)
            .await
            .expect("request failed");

        let status_code = res.status();
        let body_bytes = res
            .into_body()
            .collect()
            .await
            .expect("Read failed")
            .to_bytes();
        let body_string = String::from_utf8(body_bytes.to_vec()).expect("Conversion failed");

        assert_eq!(
            status_code, 200,
            "Expected status 200, but got {status_code}. Response body: {body_string}"
        );
    }

    /// Returns the traces that have been received by the test agent. This is not necessary in the
    /// normal course of snapshot testing, but can be useful for debugging.
    ///
    /// # Returns
    ///
    /// A `Vec` of `serde_json::Value` representing the traces that have been received by the test
    /// agent.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    /// use serde_json::to_string_pretty;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let test_agent = DatadogTestAgent::new(Some("relative/path/to/snapshot"), None, &[]).await;
    ///     let traces = test_agent.get_sent_traces().await;
    ///     let pretty_traces = to_string_pretty(&traces).expect("Failed to convert to pretty JSON");
    ///
    ///     println!("{}", pretty_traces);
    /// }
    /// ```
    pub async fn get_sent_traces(&self) -> Vec<serde_json::Value> {
        let uri = self.get_uri_for_endpoint("test/traces", None).await;

        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("Failed to create request");

        let res = self
            .agent_request_with_retry(req, 5)
            .await
            .expect("request failed");

        let body_bytes = res
            .into_body()
            .collect()
            .await
            .expect("Read failed")
            .to_bytes();

        let body_string = String::from_utf8(body_bytes.to_vec()).expect("Conversion failed");

        serde_json::from_str(&body_string).expect("Failed to parse JSON response")
    }

    /// Starts a new session with the Datadog Test Agent using the provided session token and
    /// optional sampling rates. This should be called before sending data to the test-agent to
    /// configure the session parameters. Please refer to
    /// https://github.com/DataDog/dd-apm-test-agent for more details on sessions.
    ///
    /// # Arguments
    ///
    /// * `session_token` - A string slice that holds the session token for identifying this test
    ///   session.
    /// * `agent_sample_rates_by_service` - An optional string slice that holds JSON-formatted
    ///   sampling rates by service. The format should be a JSON object mapping service and
    ///   environment pairs to sampling rates. Example: `{"service:test,env:test_env": 0.5,
    ///   "service:test2,env:prod": 0.2}`
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let test_agent = DatadogTestAgent::new(Some("relative/path/to/snapshot"), None, &[]).await;
    ///     let session_token = "test_session_token";
    ///     let sample_rates = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
    ///
    ///     test_agent
    ///         .start_session(session_token, Some(sample_rates))
    ///         .await;
    /// }
    /// ```
    pub async fn start_session(
        &self,
        session_token: &str,
        agent_sample_rates_by_service: Option<&str>,
    ) {
        // let client = hyper_migration::new_default_client();

        let mut query_params_map = HashMap::new();
        query_params_map.insert(SESSION_TEST_TOKEN_QUERY_PARAM_KEY, session_token);
        if let Some(agent_sample_rates_by_service) = agent_sample_rates_by_service {
            query_params_map.insert(SAMPLE_RATE_QUERY_PARAM_KEY, agent_sample_rates_by_service);
        }

        let uri = self
            .get_uri_for_endpoint_and_params(SESSION_START_ENDPOINT, query_params_map)
            .await;

        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("Failed to create request");

        let res = self
            .agent_request_with_retry(req, 5)
            .await
            .expect("request failed");

        assert_eq!(
            res.status(),
            200,
            "Expected status 200 for test agent {}, but got {}",
            SESSION_START_ENDPOINT,
            res.status()
        );
    }

    /// Sends an HTTP request to the Datadog Test Agent with rudimentary retry logic.
    ///
    /// In rare situations when tests are running on CI, the container running the test agent may
    /// reset the network connection even after ready states pass. Instead of adding arbitrary
    /// sleeps to these tests, we can just retry the request. This function should not be used for
    /// requests that are actually being tested, like sending payloads to the test agent. It should
    /// only be used for requests to setup the test. Examples of when you would use this
    /// function are for starting sessions or getting snapshot results.
    ///
    /// # Arguments
    ///
    /// * `req` - A `Request<Body>` representing the HTTP request to be sent.
    /// * `max_attempts` - An `i32` specifying the maximum number of request attempts to be made.
    ///
    /// # Returns
    ///
    /// * `Ok(Response<Incoming>)` - If the request succeeds. The status may or may not be
    ///   successful.
    /// * `Err(anyhow::Error)` - If all retry attempts fail or an error occurs during the request.
    ///
    /// ```
    async fn agent_request_with_retry(
        &self,
        req: Request<Body>,
        max_attempts: i32,
    ) -> anyhow::Result<Response<Incoming>> {
        let mut attempts = 1;
        let mut delay_ms = 100;
        let (parts, body) = req.into_parts();
        let body_bytes = body
            .collect()
            .await
            .expect("Failed to collect body")
            .to_bytes();
        let mut last_response;

        loop {
            let client = hyper_migration::new_default_client();
            let req = Request::from_parts(parts.clone(), Body::from_bytes(body_bytes.clone()));
            let res = client.request(req).await;

            match res {
                Ok(response) => {
                    if response.status().is_success() {
                        return Ok(response);
                    } else {
                        println!(
                            "Request failed with status code: {}. Request attempt {attempts} of {max_attempts}",
                            response.status()
                        );
                        last_response = Ok(response);
                    }
                }
                Err(e) => {
                    println!(
                        "Request failed with error: {e}. Request attempt {attempts} of {max_attempts}"
                    );
                    last_response = Err(e)
                }
            }

            if attempts >= max_attempts {
                return Ok(last_response?);
            }

            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            delay_ms *= 2;
            attempts += 1;
        }
    }

    pub async fn set_remote_config_response(&self, data: &str, snapshot_token: Option<&str>) {
        let uri = self
            .get_uri_for_endpoint(SET_REMOTE_CONFIG_RESPONSE_PATH_ENDPOINT, snapshot_token)
            .await;

        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .body(Body::from(data.as_bytes().to_vec()))
            .expect("Failed to create request");

        let res = self
            .agent_request_with_retry(req, 5)
            .await
            .expect("request failed");

        let status = res.status();
        let body = res
            .into_body()
            .collect()
            .await
            .expect("failed to read request body")
            .to_bytes();

        assert_eq!(
            status.as_u16(),
            202,
            "Expected status 200 for test agent {}, but got {}: {:?}",
            SET_REMOTE_CONFIG_RESPONSE_PATH_ENDPOINT,
            status.as_u16(),
            String::from_utf8_lossy(&body)
        );
    }
}
