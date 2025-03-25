// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use cargo_metadata::MetadataCommand;
use ddcommon::hyper_migration;
use http_body_util::BodyExt;
use hyper::Uri;
use testcontainers::core::AccessMode;
use testcontainers::{
    core::{ports::ContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
    *,
};

const TEST_AGENT_IMAGE_NAME: &str = "ghcr.io/datadog/dd-apm-test-agent/ddapm-test-agent";
const TEST_AGENT_IMAGE_TAG: &str = "latest";
const TEST_AGENT_READY_MSG: &str =
    "INFO:ddapm_test_agent.agent:Trace request stall seconds setting set to 0.0.";

const TEST_AGENT_PORT: ContainerPort = ContainerPort::Udp(8126);
const SAMPLE_RATE_QUERY_PARAM_KEY: &str = "agent_sample_rate_by_service";
const SESSION_TEST_TOKEN_QUERY_PARAM_KEY: &str = "test_session_token";
const SESSION_START_ENDPOINT: &str = "test/session/start";

#[derive(Debug)]
struct DatadogTestAgentContainer {
    mounts: Vec<Mount>,
    env_vars: HashMap<String, String>,
    ports: [ContainerPort; 1],
}

impl Image for DatadogTestAgentContainer {
    fn name(&self) -> &str {
        TEST_AGENT_IMAGE_NAME
    }

    fn tag(&self) -> &str {
        TEST_AGENT_IMAGE_TAG
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![
            WaitFor::message_on_stderr(TEST_AGENT_READY_MSG),
            // This wait is in place because on some github runners the docker container may reset
            // the connection even though the test-agent stderr indicates it is ready.
            // TODO: Investigate if we can emit a message from the test-agent when it is truly
            // ready.
            WaitFor::Duration {
                length: Duration::from_secs(1),
            },
        ]
    }

    fn mounts(&self) -> impl IntoIterator<Item = &Mount> {
        self.mounts.iter()
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &self.ports
    }

    fn env_vars(
        &self,
    ) -> impl IntoIterator<Item = (impl Into<Cow<'_, str>>, impl Into<Cow<'_, str>>)> {
        self.env_vars.iter()
    }
}

impl DatadogTestAgentContainer {
    fn new(relative_snapshot_path: Option<&str>, absolute_socket_path: Option<&str>) -> Self {
        let mut env_vars = HashMap::new();
        let mut mounts = Vec::new();

        if let Some(absolute_socket_path) = absolute_socket_path {
            env_vars.insert(
                "DD_APM_RECEIVER_SOCKET".to_string(),
                "/tmp/ddsockets/apm.socket".to_owned(),
            );

            mounts.push(
                Mount::bind_mount(absolute_socket_path, "/tmp/ddsockets")
                    .with_access_mode(AccessMode::ReadWrite),
            );
        }

        if let Some(relative_snapshot_path) = relative_snapshot_path {
            mounts.push(
                Mount::bind_mount(
                    DatadogTestAgentContainer::calculate_volume_absolute_path(
                        relative_snapshot_path,
                    ),
                    "/snapshots",
                )
                .with_access_mode(AccessMode::ReadWrite),
            );
        }

        let ports = [TEST_AGENT_PORT];
        DatadogTestAgentContainer {
            mounts,
            env_vars,
            ports,
        }
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
/// use ddcommon::Endpoint;
///
/// use tokio;
///
/// #[tokio::main]
/// async fn main() {
///     // Create a new DatadogTestAgent instance
///     let test_agent = DatadogTestAgent::new(Some("relative/path/to/snapshot"), None).await;
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
///     let _result = data.send().await;
///
///     // Assert that the snapshot for a given token matches the expected snapshot
///     test_agent.assert_snapshot("snapshot-token").await;
/// }
/// ```
pub struct DatadogTestAgent {
    container: ContainerAsync<DatadogTestAgentContainer>,
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
    /// # Returns
    ///
    /// A new `DatadogTestAgent`.
    pub async fn new(
        relative_snapshot_path: Option<&str>,
        absolute_socket_path: Option<&str>,
    ) -> Self {
        DatadogTestAgent {
            container: DatadogTestAgentContainer::new(relative_snapshot_path, absolute_socket_path)
                .start()
                .await
                .expect("Unable to start DatadogTestAgent, is the Docker Daemon running?"),
        }
    }

    async fn get_base_uri_string(&self) -> String {
        let container_host = self.container.get_host().await.unwrap().to_string();
        let container_port = self
            .container
            .get_host_port_ipv4(TEST_AGENT_PORT)
            .await
            .unwrap();

        format!("http://{}:{}", container_host, container_port)
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
            Some(token) => format!(
                "{}/{}?test_session_token={}",
                base_uri_string, endpoint, token
            ),
            None => format!("{}/{}", base_uri_string, endpoint),
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
            format!("?{}", query)
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
        let client = hyper_migration::new_default_client();
        let uri = self
            .get_uri_for_endpoint("test/session/snapshot", Some(snapshot_token))
            .await;
        let res = client.get(uri).await.expect("Request failed");
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
            "Expected status 200, but got {}. Response body: {}",
            status_code, body_string
        );
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
    /// use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let test_agent = DatadogTestAgent::new(Some("relative/path/to/snapshot"), None).await;
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
        let client = hyper_migration::new_default_client();

        let mut query_params_map = HashMap::new();
        query_params_map.insert(SESSION_TEST_TOKEN_QUERY_PARAM_KEY, session_token);
        if let Some(agent_sample_rates_by_service) = agent_sample_rates_by_service {
            query_params_map.insert(SAMPLE_RATE_QUERY_PARAM_KEY, agent_sample_rates_by_service);
        }

        let uri = self
            .get_uri_for_endpoint_and_params(SESSION_START_ENDPOINT, query_params_map)
            .await;

        let res = client.get(uri).await.expect("Request failed");

        assert_eq!(
            res.status(),
            200,
            "Expected status 200 for test agent {}, but got {}",
            SESSION_START_ENDPOINT,
            res.status()
        );
    }
}
