// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// The `StatsPayload.agent_version` value: the tracer version passed to the
/// exporter suffixed with the language, so the backend can tell libdatadog
/// tracers from the Agent.
pub fn agentless_stats_version(tracer_version: &str, language: &str) -> String {
    format!("{tracer_version}-{language}")
}
