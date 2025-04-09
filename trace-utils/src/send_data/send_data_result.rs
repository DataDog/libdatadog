// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::send_with_retry::{SendWithRetryError, SendWithRetryResult};
use ddcommon::hyper_migration;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct SendDataResult {
    /// Keeps track of the last successful response.
    pub last_success_response: Option<hyper_migration::HttpResponse>,
    /// Keeps track of the last error.
    pub last_error: Option<SendWithRetryError>,
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
    /// Count metric for 'trace_chunk_sent'
    pub chunks_sent: u64,
    /// Count metric for 'trace_chunks_dropped'
    pub chunks_dropped: u64,
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
                self.last_success_response = Some(response);
                self.requests_count += u64::from(attempts);
            }
            Err(err) => {
                match err {
                    SendWithRetryError::Http(ref response, attempts) => {
                        let status_code = response.status().as_u16();
                        self.errors_status_code += 1;
                        *self
                            .responses_count_per_code
                            .entry(status_code)
                            .or_default() += 1;
                        self.chunks_dropped += chunks;
                        self.requests_count += u64::from(attempts);
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
                };
                self.last_error = Some(err);
            }
        }
    }
}
