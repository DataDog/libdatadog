// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use cargo_metadata::MetadataCommand;
use hyper::body::HttpBody;
use hyper::{Client, Uri};
use testcontainers::core::AccessMode;
use testcontainers::{
    core::{Mount, WaitFor},
    runners::AsyncRunner,
    *,
};

const TEST_AGENT_IMAGE_NAME: &str = "ghcr.io/datadog/dd-apm-test-agent/ddapm-test-agent";
const TEST_AGENT_IMAGE_TAG: &str = "latest";
const TEST_AGENT_READY_MSG: &str =
    "INFO:ddapm_test_agent.agent:Trace request stall seconds setting set to 0.0.";

const TEST_AGENT_PORT: u16 = 8126;

#[derive(Debug)]
struct DatadogTestAgentContainer {
    mounts: Vec<Mount>,
}

impl Image for DatadogTestAgentContainer {
    type Args = Vec<String>;

    fn name(&self) -> String {
        TEST_AGENT_IMAGE_NAME.to_owned()
    }

    fn tag(&self) -> String {
        TEST_AGENT_IMAGE_TAG.to_owned()
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

    fn mounts(&self) -> Box<dyn Iterator<Item = &Mount> + '_> {
        Box::new(self.mounts.iter())
    }
    fn expose_ports(&self) -> Vec<u16> {
        vec![TEST_AGENT_PORT]
    }
}

impl DatadogTestAgentContainer {
    fn new(relative_snapshot_path: Option<&str>) -> Self {
        if let Some(relative_snapshot_path) = relative_snapshot_path {
            let mount = Mount::bind_mount(
                DatadogTestAgentContainer::calculate_snapshot_absolute_path(relative_snapshot_path),
                "/snapshots",
            )
            .with_access_mode(AccessMode::ReadWrite);

            DatadogTestAgentContainer {
                mounts: vec![mount],
            }
        } else {
            DatadogTestAgentContainer { mounts: vec![] }
        }
    }
    // The docker image requires an absolute path when mounting a volume. This function gets the
    // absolute path of the workspace and appends the provided relative path.
    fn calculate_snapshot_absolute_path(relative_snapshot_path: &str) -> String {
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
/// use ddcommon_net1::Endpoint;
///
/// use tokio;
///
/// #[tokio::main]
/// async fn main() {
///     // Create a new DatadogTestAgent instance
///     let test_agent = DatadogTestAgent::new(Some("relative/path/to/snapshot")).await;
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
    /// # Returns
    ///
    /// A new `DatadogTestAgent`.
    pub async fn new(relative_snapshot_path: Option<&str>) -> Self {
        DatadogTestAgent {
            container: DatadogTestAgentContainer::new(relative_snapshot_path)
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

    /// Asserts that the snapshot for a given token matches the expected snapshot. This should be
    /// called after sending data to the test-agent with the same token.
    ///
    /// # Arguments
    ///
    /// * `snapshot_token` - A string slice that holds the snapshot token.
    pub async fn assert_snapshot(&self, snapshot_token: &str) {
        let client = Client::new();
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
}
