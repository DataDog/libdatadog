// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::agent_client::AgentClient;

pub struct TraceExporter {
    host: String,
    port: u16,
    client: AgentClient,
}

impl TraceExporter {
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    pub fn send(&mut self, data: &[u8], trace_count: usize) -> Result<String, String> {
        let url = format!("{}:{}{}", self.host, self.port, "/v0.4/traces");
        self.client.blocking_send_trace(&url, data.to_vec(), trace_count)
    }
}

#[derive(Default)]
pub struct TraceExporterBuilder {
    host: Option<String>,
    port: Option<u16>,
    timeout: Option<u64>,
    tracer_version: Option<String>,
    language: Option<String>,
    language_version: Option<String>,
    interpreter: Option<String>,
}

impl TraceExporterBuilder {
    pub fn set_timeout(&mut self, timeout: u64) -> &mut TraceExporterBuilder {
        self.timeout = Some(timeout);
        self
    }

    pub fn set_host(&mut self, host: &str) -> &mut TraceExporterBuilder {
        self.host = Some(String::from(host));
        self
    }

    pub fn set_port(&mut self, port: u16) -> &mut TraceExporterBuilder {
        self.port = Some(port);
        self
    }

    pub fn set_tracer_version(&mut self, tracer_version: &str) -> &mut TraceExporterBuilder {
        self.tracer_version = Some(String::from(tracer_version));
        self
    }

    pub fn set_language(&mut self, lang: &str) -> &mut TraceExporterBuilder {
        self.language = Some(String::from(lang));
        self
    }

    pub fn set_language_version(&mut self, lang_version: &str) -> &mut TraceExporterBuilder {
        self.language_version = Some(String::from(lang_version));
        self
    }

    pub fn set_language_interpreter(
        &mut self,
        lang_interpreter: &str,
    ) -> &mut TraceExporterBuilder {
        self.interpreter = Some(String::from(lang_interpreter));
        self
    }

    pub fn build(&mut self) -> TraceExporter {
        TraceExporter {
            client: AgentClient::new(
                self.timeout.unwrap_or_default(),
                self.tracer_version.as_ref().unwrap(),
                self.language.as_ref().unwrap(),
                self.language_version.as_ref().unwrap(),
                self.interpreter.as_ref().unwrap(),
            ),
            // TODO: avoid cloning?
            host: self.host.clone().unwrap(),
            port: self.port.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new() {
        let mut builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_timeout(10)
            .set_host("http://127.0.0.1")
            .set_port(8127)
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .build();

        assert_eq!(exporter.host, "http://127.0.0.1");
        assert_eq!(exporter.port, 8127);

        assert_eq!(builder.timeout.unwrap(), 10);
        assert_eq!(builder.host.unwrap(), "http://127.0.0.1");
        assert_eq!(builder.port.unwrap(), 8127);
        assert_eq!(builder.tracer_version.unwrap(), "v0.1");
        assert_eq!(builder.language.unwrap(), "nodejs");
        assert_eq!(builder.language_version.unwrap(), "1.0");
        assert_eq!(builder.interpreter.unwrap(), "v8");
    }

    #[test]
    fn configure() {}
    #[test]
    fn export() {}
}
