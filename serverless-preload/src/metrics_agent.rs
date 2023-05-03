// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_metrics::agent::*;
use datadog_metrics::config::ConfigBuilder;

#[tokio::main]
async fn main() {
    let api_key = std::env::var("DD_API_KEY").expect("Must provide DD_API_KEY");
    let site = std::env::var("DD_SITE").unwrap_or(String::from("us1.datadoghq.com"));
    let config = ConfigBuilder::default()
        .api_key(api_key)
        .site(site)
        .build()
        .expect("Error constructing metrics agent");

    let metrics_agent = MetricsAgent::with_config(config);

    metrics_agent.run().await;
}
