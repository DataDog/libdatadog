// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// The libdatadog version reported in the `StatsPayload.agent_version` field.
///
/// Semver with a `-libdatadog` suffix so the backend can distinguish stats sent
/// by libdatadog from those sent by the Datadog Agent.
pub fn agentless_stats_version() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), "-libdatadog")
}
