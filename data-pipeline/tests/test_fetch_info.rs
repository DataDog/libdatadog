// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracing_integration_tests {
    use data_pipeline::agent_info::{AgentInfoCell};
    use data_pipeline::agent_info::{fetch_info, AgentInfoFetcher};
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use ddcommon::{worker::Worker, Endpoint};
    use std::time::Duration;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_fetch_info_from_test_agent() {
        let test_agent = DatadogTestAgent::new(None, None, &[]).await;
        let endpoint = Endpoint::from_url(test_agent.get_uri_for_endpoint("info", None).await);
        let info = fetch_info(&endpoint)
            .await
            .expect("Failed to fetch agent info");
        assert!(
            info.info
                .version
                .expect("Missing version field in agent response")
                == "test"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_agent_info_fetcher_with_test_agent() {
        let test_agent = DatadogTestAgent::new(None, None, &[]).await;
        let endpoint = Endpoint::from_url(test_agent.get_uri_for_endpoint("info", None).await);
        let agent_info = AgentInfoCell::default();
        let (mut fetcher, _response_observer) =
            AgentInfoFetcher::new(endpoint, Duration::from_secs(1), agent_info.clone());
        tokio::spawn(async move { fetcher.run().await });
        let info_received = async {
            while agent_info.load().is_none() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            agent_info.load().as_deref().unwrap().clone()
        };

        let info = tokio::time::timeout(Duration::from_secs(10), info_received)
            .await
            .expect("Agent request timed out");

        assert!(
            info.info
                .version
                .as_ref()
                .expect("Missing version field in agent response")
                == "test"
        );
    }
}
