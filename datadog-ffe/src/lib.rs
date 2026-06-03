// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod flag_type;

pub mod remote_config;
pub mod rules_based;
#[cfg(any(feature = "exposure-events", feature = "evaluation-metrics"))]
pub mod telemetry;

pub use flag_type::{ExpectedFlagType, FlagType};
