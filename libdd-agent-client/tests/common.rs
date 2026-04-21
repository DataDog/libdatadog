// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use httpmock::MockServer;
use libdd_agent_client::{AgentClient, LanguageMetadata};

pub fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn client_for(server: &MockServer) -> AgentClient {
    ensure_crypto_provider();
    AgentClient::builder()
        .http("localhost", server.port())
        .language_metadata(LanguageMetadata::new(
            "python", "3.12.1", "CPython", "2.18.0",
        ))
        .build()
        .expect("client build failed")
}
