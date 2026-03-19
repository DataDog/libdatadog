// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ureq_client::UreqClient;
use http::StatusCode;

#[derive(Debug)]
pub(crate) struct PreparedRequest {
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct ProfileTransport {
    client: UreqClient,
}

impl ProfileTransport {
    pub(crate) fn new(
        endpoint: libdd_common::ResolvedEndpoint,
        tls_config: Option<ureq::tls::TlsConfig>,
    ) -> anyhow::Result<Self> {
        let client = UreqClient::new(endpoint, tls_config)?;
        Ok(Self { client })
    }

    pub(crate) fn send(&self, request: PreparedRequest) -> anyhow::Result<StatusCode> {
        let status = self.client.send(request)?;
        Ok(status)
    }
}

impl std::fmt::Debug for ProfileTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileTransport").finish()
    }
}
