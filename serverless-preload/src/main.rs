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
