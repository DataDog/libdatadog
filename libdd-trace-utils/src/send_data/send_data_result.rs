// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::send_with_retry::{SendWithRetryError, SendWithRetryResult};
use anyhow::anyhow;
use libdd_common::hyper_migration;
use std::collections::HashMap;

#[derive(Debug)]
pub struct SendDataResult {
    /// Keeps track of the last request result.
    pub last_result: anyhow::Result<hyper_migration::HttpResponse>,
    /// Count metric for 'trace_api.requests'.
    pub requests_count: u64,
    /// Count metric for 'trace_api.responses'. Each key maps a different HTTP status code.
    pub responses_count_per_code: HashMap<u16, u64>,
    /// Count metric for 'trace_api.errors' (type: timeout).
    pub errors_timeout: u64,
    /// Count metric for 'trace_api.errors' (type: network).
    pub errors_network: u64,
    /// Count metric for 'trace_api.errors' (type: status_code).
    pub errors_status_code: u64,
    /// Count metric for 'trace_api.bytes'
    pub bytes_sent: u64,
    /// Count metric for 'trace_chunks_sent'
    pub chunks_sent: u64,
    /// Count metric for 'trace_chunks_dropped'
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
    /// Updates [`SendDataResult`] internal information with the request's result information.
    ///
    /// # Arguments
    ///
    /// * `res` -  [`SendWithRetryResult`].
    /// * `bytes_sent` -  Number of bytes in the payload sent.
    /// * `chunks` -  Number of chunks sent or dropped in the request.
    pub(crate) fn update(&mut self, res: SendWithRetryResult, bytes_sent: u64, chunks: u64) {
        match res {
            Ok((response, attempts)) => {
                *self
                    .responses_count_per_code
                    .entry(response.status().as_u16())
                    .or_default() += 1;
                self.bytes_sent += bytes_sent;
                self.chunks_sent += chunks;
                self.last_result = Ok(response);
                self.requests_count += u64::from(attempts);
            }
            Err(err) => match err {
                SendWithRetryError::Http(response, attempts) => {
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
                SendWithRetryError::Timeout(attempts) => {
                    self.errors_timeout += 1;
                    self.chunks_dropped += chunks;
                    self.requests_count += u64::from(attempts);
                }
                SendWithRetryError::Network(_, attempts) => {
                    self.errors_network += 1;
                    self.chunks_dropped += chunks;
                    self.requests_count += u64::from(attempts);
                }
                SendWithRetryError::Build(attempts) => {
                    self.chunks_dropped += chunks;
                    self.requests_count += u64::from(attempts);
                }
            },
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
