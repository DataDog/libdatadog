// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::aggregator::Aggregator;
use crate::datadog;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::debug;

pub struct Flusher {
    dd_api: datadog::DdApi,
    aggregator: Arc<Mutex<Aggregator>>,
}

#[inline]
#[must_use]
pub fn build_fqdn_metrics(site: String) -> String {
    format!("https://api.{site}")
}

#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(
        api_key: String,
        aggregator: Arc<Mutex<Aggregator>>,
        site: String,
        https_proxy: Option<String>,
        timeout: Duration,
    ) -> Self {
        let dd_api = datadog::DdApi::new(api_key, site, https_proxy, timeout);
        Flusher { dd_api, aggregator }
    }

    pub async fn flush(&mut self) {
        let (all_series, all_distributions) = {
            let mut aggregator = self.aggregator.lock().expect("lock poisoned");
            (
                aggregator.consume_metrics(),
                aggregator.consume_distributions(),
            )
        };

        let n_series = all_series.len();
        let n_distributions = all_distributions.len();

        debug!("Flushing {n_series} series and {n_distributions} distributions");

        // TODO: client timeout is for each invocation, so NxM times with N time series batches and
        // M distro batches
        for a_batch in all_series {
            self.dd_api.ship_series(&a_batch).await;
            // TODO(astuyve) retry and do not panic
        }
        for a_batch in all_distributions {
            self.dd_api.ship_distributions(&a_batch).await;
        }
    }
}
