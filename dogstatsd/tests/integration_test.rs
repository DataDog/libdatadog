// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use dogstatsd::metric::SortedTags;
use dogstatsd::{
    aggregator::Aggregator as MetricsAggregator,
    constants::CONTEXTS,
    dogstatsd::{DogStatsD, DogStatsDConfig},
    flusher::Flusher,
};
use mockito::Server;
use std::sync::{Arc, Mutex};
use tokio::{
    net::UdpSocket,
    time::{sleep, timeout, Duration},
};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[cfg(not(miri))]
#[tokio::test]
async fn dogstatsd_server_ships_series() {
    let mut mock_server = Server::new_async().await;

    let mock = mock_server
        .mock("POST", "/api/v2/series")
        .match_header("DD-API-KEY", "mock-api-key")
        .match_header("Content-Type", "application/json")
        .with_status(202)
        .create_async()
        .await;

    let metrics_aggr = Arc::new(Mutex::new(
        MetricsAggregator::new(SortedTags::parse("sometkey:somevalue").unwrap(), CONTEXTS)
            .expect("failed to create aggregator"),
    ));

    let _ = start_dogstatsd(&metrics_aggr).await;

    let mut metrics_flusher = Flusher::new(
        "mock-api-key".to_string(),
        Arc::clone(&metrics_aggr),
        mock_server.url(),
        None,
        None,
    );

    let server_address = "127.0.0.1:18125";
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("unable to bind UDP socket");
    let metric = "custom_metric:1|g";

    socket
        .send_to(metric.as_bytes(), &server_address)
        .await
        .expect("unable to send metric");

    let flush = async {
        while !mock.matched() {
            sleep(Duration::from_millis(100)).await;
            metrics_flusher.flush().await;
        }
    };

    let result = timeout(Duration::from_millis(1000), flush).await;

    match result {
        Ok(_) => mock.assert(),
        Err(_) => panic!("timed out before server received metric flush"),
    }
}

async fn start_dogstatsd(metrics_aggr: &Arc<Mutex<MetricsAggregator>>) -> CancellationToken {
    let dogstatsd_config = DogStatsDConfig {
        host: "127.0.0.1".to_string(),
        port: 18125,
    };
    let dogstatsd_cancel_token = tokio_util::sync::CancellationToken::new();
    let dogstatsd_client = DogStatsD::new(
        &dogstatsd_config,
        Arc::clone(metrics_aggr),
        dogstatsd_cancel_token.clone(),
    )
    .await;

    tokio::spawn(async move {
        dogstatsd_client.spin().await;
    });

    dogstatsd_cancel_token
}
