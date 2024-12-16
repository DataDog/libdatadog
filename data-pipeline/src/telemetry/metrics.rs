// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provides an abstraction layer to hold metrics that comes from 'SendDataResult'.
use datadog_trace_utils::trace_utils::SendDataResult;
use std::collections::HashMap;

/// Structure to accumulate partial results coming from sending traces to the agent.
#[derive(Default)]
pub struct Metrics {
    /// Holds 'trace_api.requests' metric.
    pub api_requests: u64,
    /// Holds 'trace_api.responses' metric.
    pub api_responses_count_per_code: HashMap<u16, u64>,
    /// Holds 'trace_api.errors' metric due to timeout expirations.
    pub api_errors_timeout: u64,
    /// Holds 'trace_api.errors' metric due to network issues.
    pub api_errors_network: u64,
    /// Holds 'trace_api.errors' metric due to http issues.
    pub api_errors_status_code: u64,
    /// Holds 'trace_api.bytes' metric.
    pub bytes_sent: u64,
    /// Holds 'trace_chunk_sent' metric.
    pub chunks_sent: u64,
    /// Holds 'trace_chunk_dropped' metric.
    pub chunks_dropped: u64,
}

impl Metrics {
    /// Updates the metric internal properties based on `result` contents.
    pub fn update(&mut self, result: &SendDataResult) {
        self.api_requests += result.requests_count;
        self.api_errors_timeout += result.errors_timeout;
        self.api_errors_network += result.errors_network;
        self.api_errors_status_code += result.errors_status_code;
        self.bytes_sent += result.bytes_sent;
        self.chunks_sent += result.chunks_sent;
        self.chunks_dropped += result.chunks_dropped;

        for (status_code, count) in &result.responses_count_per_code {
            *self
                .api_responses_count_per_code
                .entry(*status_code)
                .or_default() += count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_test() {
        let mut result = SendDataResult::default();
        let mut metrics = Metrics::default();

        assert_eq!(metrics.api_requests, 0);
        assert_eq!(metrics.api_errors_timeout, 0);
        assert_eq!(metrics.api_errors_network, 0);
        assert_eq!(metrics.api_errors_status_code, 0);
        assert_eq!(metrics.bytes_sent, 0);
        assert_eq!(metrics.chunks_dropped, 0);
        assert!(metrics.api_responses_count_per_code.is_empty());

        metrics.update(&result);
        assert_eq!(metrics.api_requests, 0);
        assert_eq!(metrics.api_errors_timeout, 0);
        assert_eq!(metrics.api_errors_network, 0);
        assert_eq!(metrics.api_errors_status_code, 0);
        assert_eq!(metrics.bytes_sent, 0);
        assert_eq!(metrics.chunks_dropped, 0);
        assert!(metrics.api_responses_count_per_code.is_empty());

        result.requests_count = 1;
        result.chunks_dropped = 1;
        result.bytes_sent = 1;
        result.errors_timeout = 1;
        result.errors_network = 1;
        result.errors_status_code = 1;
        result.responses_count_per_code.insert(200, 1);

        metrics.update(&result);
        assert_eq!(metrics.api_requests, 1);
        assert_eq!(metrics.api_errors_timeout, 1);
        assert_eq!(metrics.api_errors_network, 1);
        assert_eq!(metrics.api_errors_status_code, 1);
        assert_eq!(metrics.bytes_sent, 1);
        assert_eq!(metrics.chunks_dropped, 1);
        assert_eq!(metrics.api_responses_count_per_code.len(), 1);
        assert_eq!(
            *metrics.api_responses_count_per_code.get(&200_u16).unwrap(),
            1
        );

        metrics.update(&result);
        assert_eq!(metrics.api_requests, 2);
        assert_eq!(metrics.api_errors_timeout, 2);
        assert_eq!(metrics.api_errors_network, 2);
        assert_eq!(metrics.api_errors_status_code, 2);
        assert_eq!(metrics.bytes_sent, 2);
        assert_eq!(metrics.chunks_dropped, 2);
        assert_eq!(metrics.api_responses_count_per_code.len(), 1);
        assert_eq!(
            *metrics.api_responses_count_per_code.get(&200_u16).unwrap(),
            2
        );
    }
}
