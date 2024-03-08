// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;
use ureq::{Agent, AgentBuilder};

#[derive(Debug, Clone)]
struct AgentClientError;

#[derive(Clone)]
struct MetaInfo {
    tracer_version: String,
    language: String,
    language_version: String,
    language_interpreter: String,
}

#[derive(Clone)]
pub struct AgentClient {
    client: Agent,
    // TODO: Store headers in a map.
    meta_info: MetaInfo,
}

impl AgentClient {
    pub fn new(
        timeout: u64,
        tracer_version: &str,
        lang: &str,
        lang_version: &str,
        lang_interpreter: &str,
    ) -> AgentClient {
        AgentClient {
            client: AgentBuilder::new()
                .timeout_read(Duration::from_millis(timeout))
                .timeout_write(Duration::from_millis(timeout))
                .build(),
            meta_info: MetaInfo {
                tracer_version: String::from(tracer_version),
                language: String::from(lang),
                language_version: String::from(lang_version),
                language_interpreter: String::from(lang_interpreter),
            },
        }
    }

    pub fn blocking_send_trace(
        &mut self,
        url: &str,
        payload: Vec<u8>,
        trace_count: usize,
    ) -> Result<String, String> {
        let result = self
            .client
            .post(url)
            .set("Content-Type", "application/msgpack")
            .set("Datadog-Meta-Lang", &self.meta_info.language)
            .set("Datadog-Meta-Version", &self.meta_info.language_interpreter)
            .set("Datadog-Meta-Interpreter", &self.meta_info.language_version)
            .set(
                "Datadog-Meta-Tracer-Version",
                &self.meta_info.tracer_version,
            )
            .set("X-Datadog-Trace-Count", &trace_count.to_string())
            .send_bytes(&payload)
            .map_err(|err| format!("Error {}", err))?
            .into_string()
            .map_err(|err| format!("Error {}", err))?;

        Ok(result)
    }
}
