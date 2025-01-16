// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracing_integration_tests {
    use arc_swap::access::Access;
    use data_pipeline::agent_info::{fetch_info, AgentInfoFetcher};
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use ddcommon_net1::Endpoint;
    use std::time::Duration;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_fetch_info_from_test_agent() {
        let test_agent = DatadogTestAgent::new(None).await;
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
        let test_agent = DatadogTestAgent::new(None).await;
        let endpoint = Endpoint::from_url(test_agent.get_uri_for_endpoint("info", None).await);
        let fetcher = AgentInfoFetcher::new(endpoint, Duration::from_secs(1));
        let info_arc = fetcher.get_info();
        tokio::spawn(async move { fetcher.run().await });
        let info_received = async {
            while info_arc.load().is_none() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            info_arc.load()
        };

        let info = tokio::time::timeout(Duration::from_secs(10), info_received)
            .await
            .expect("Agent request timed out");

        assert!(
            info.as_ref()
                .unwrap()
                .info
                .version
                .clone()
                .expect("Missing version field in agent response")
                == "test"
        );
    }
}
