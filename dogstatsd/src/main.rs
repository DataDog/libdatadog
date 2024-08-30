// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use dogstatsd::aggregator::Aggregator;
use dogstatsd::dogstatsd::{DogStatsD, DogStatsDConfig};
use dogstatsd::flusher::Flusher;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let metrics_aggr = Arc::new(Mutex::new(
        Aggregator::new(Vec::new(), 1_024).expect("failed to create aggregator"),
    ));
    let _ = start_dogstatsd(Arc::clone(&metrics_aggr)).await;

    let mut metrics_flusher = Flusher::new(
        "an_api_key".to_string(),
        Arc::clone(&metrics_aggr),
        "datadoghq.com".to_string(),
    );

    thread::sleep(Duration::from_secs(5));
    metrics_flusher.flush().await;
}

async fn start_dogstatsd(metrics_aggr: Arc<Mutex<Aggregator>>) -> CancellationToken {
    let dogstatsd_config = DogStatsDConfig {
        host: "0.0.0.0".to_string(),
        port: 8125,
    };
    let dogstatsd_cancel_token = tokio_util::sync::CancellationToken::new();
    let dogstatsd_client = DogStatsD::new(
        &dogstatsd_config,
        Arc::clone(&metrics_aggr),
        dogstatsd_cancel_token.clone(),
    )
    .await;

    tokio::spawn(async move {
        dogstatsd_client.spin().await;
    });

    dogstatsd_cancel_token
}
