// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::send_data::RequestResult;
use anyhow::anyhow;
use hyper::{Body, Response};
use std::collections::HashMap;

#[derive(Debug)]
pub struct SendDataResult {
    // Keeps track of the last request result.
    pub last_result: anyhow::Result<Response<Body>>,
    // Count metric for 'trace_api.requests'.
    pub requests_count: u64,
    // Count metric for 'trace_api.responses'. Each key maps  a different HTTP status code.
    pub responses_count_per_code: HashMap<u16, u64>,
    // Count metric for 'trace_api.errors' (type: timeout).
    pub errors_timeout: u64,
    // Count metric for 'trace_api.errors' (type: network).
    pub errors_network: u64,
    // Count metric for 'trace_api.errors' (type: status_code).
    pub errors_status_code: u64,
    // Count metric for 'trace_api.bytes'
    pub bytes_sent: u64,
    // Count metric for 'trace_chunk_sent'
    pub chunks_sent: u64,
    // Count metric for 'trace_chunks_dropped'
    pub chunks_dropped: u64,
}

impl Default for SendDataResult {
    fn default() -> Self {
        SendDataResult {
            last_result: Err(anyhow!("No requests sent")),
            requests_count: 0,
            responses_count_per_code: Default::default(),
            errors_timeout: 0,
            errors_network: 0,
            errors_status_code: 0,
            bytes_sent: 0,
            chunks_sent: 0,
            chunks_dropped: 0,
        }
    }
}

impl SendDataResult {
    ///
    /// Updates `SendDataResult` internal information with the request's result information.
    ///
    /// # Arguments
    ///
    /// * `res` - Request result.
    pub(crate) async fn update(&mut self, res: RequestResult) {
        match res {
            RequestResult::Success((response, attempts, bytes, chunks)) => {
                *self
                    .responses_count_per_code
                    .entry(response.status().as_u16())
                    .or_default() += 1;
                self.bytes_sent += bytes;
                self.chunks_sent += chunks;
                self.last_result = Ok(response);
                self.requests_count += u64::from(attempts);
            }
            RequestResult::Error((response, attempts, chunks)) => {
                let status_code = response.status().as_u16();
                self.errors_status_code += 1;
                *self
                    .responses_count_per_code
                    .entry(status_code)
                    .or_default() += 1;
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);
                self.last_result = Ok(response);
            }
            RequestResult::TimeoutError((attempts, chunks)) => {
                self.errors_timeout += 1;
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);
            }
            RequestResult::NetworkError((attempts, chunks)) => {
                self.errors_network += 1;
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);
            }
            RequestResult::BuildError((attempts, chunks)) => {
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);
            }
        }
    }

    ///
    /// Sets `SendDataResult` last result information.
    /// expected result.
    ///
    /// # Arguments
    ///
    /// * `err` - Error to be set.
    pub(crate) fn error(mut self, err: anyhow::Error) -> SendDataResult {
        self.last_result = Err(err);
        self
    }
}
