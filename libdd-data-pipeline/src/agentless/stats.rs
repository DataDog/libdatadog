// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// The version reported in the `StatsPayload.agent_version` field.
///
/// Uses the tracer/library version passed to the trace exporter, suffixed with
/// the language, so the backend can distinguish stats sent by libdatadog-based
/// tracers from those sent by the Datadog Agent.
pub fn agentless_stats_version(tracer_version: &str, language: &str) -> String {
    format!("{tracer_version}-{language}")
}
