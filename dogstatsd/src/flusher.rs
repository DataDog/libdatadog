// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::aggregator::Aggregator;
use crate::datadog;
use datadog::{DdApi, MetricsIntakeUrlPrefix};
use reqwest::{Response, StatusCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error};

pub struct Flusher {
    dd_api: DdApi,
    aggregator: Arc<Mutex<Aggregator>>,
}

pub struct FlusherConfig {
    pub api_key: String,
    pub aggregator: Arc<Mutex<Aggregator>>,
    pub metrics_intake_url_prefix: MetricsIntakeUrlPrefix,
    pub https_proxy: Option<String>,
    pub timeout: Duration,
}

#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(config: FlusherConfig) -> Self {
        let dd_api = DdApi::new(
            config.api_key,
            config.metrics_intake_url_prefix,
            config.https_proxy,
            config.timeout,
        );
        Flusher {
            dd_api,
            aggregator: config.aggregator,
        }
    }

    pub async fn flush(&mut self) {
        let (all_series, all_distributions) = {
            #[allow(clippy::expect_used)]
            let mut aggregator = self.aggregator.lock().expect("lock poisoned");
            (
                aggregator.consume_metrics(),
                aggregator.consume_distributions(),
            )
        };

        let n_series = all_series.len();
        let n_distributions = all_distributions.len();

        debug!("Flushing {n_series} series and {n_distributions} distributions");

        let dd_api_clone = self.dd_api.clone();
        let series_handle = tokio::spawn(async move {
            for a_batch in all_series {
                let continue_shipping =
                    should_try_next_batch(dd_api_clone.ship_series(&a_batch).await).await;
                if !continue_shipping {
                    break;
                }
            }
        });
        let dd_api_clone = self.dd_api.clone();
        let distributions_handle = tokio::spawn(async move {
            for a_batch in all_distributions {
                let continue_shipping =
                    should_try_next_batch(dd_api_clone.ship_distributions(&a_batch).await).await;
                if !continue_shipping {
                    break;
                }
            }
        });

        match tokio::try_join!(series_handle, distributions_handle) {
            Ok(_) => {
                debug!("Successfully flushed {n_series} series and {n_distributions} distributions")
            }
            Err(err) => {
                error!("Failed to flush metrics{err}")
            }
        };
    }
}

pub enum ShippingError {
    Payload(String),
    Destination(Option<StatusCode>, String),
}

async fn should_try_next_batch(resp: Result<Response, ShippingError>) -> bool {
    match resp {
        Ok(resp_payload) => match resp_payload.status() {
            StatusCode::ACCEPTED => true,
            unexpected_status_code => {
                error!(
                    "{}: Failed to push to API: {:?}",
                    unexpected_status_code,
                    resp_payload.text().await.unwrap_or_default()
                );
                true
            }
        },
        Err(ShippingError::Payload(msg)) => {
            error!("Failed to prepare payload. Data dropped: {}", msg);
            true
        }
        Err(ShippingError::Destination(sc, msg)) => {
            error!("Error shipping data: {:?} {}", sc, msg);
            false
        }
    }
}
