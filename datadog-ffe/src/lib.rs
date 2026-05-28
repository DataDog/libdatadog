// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod flag_type;

pub mod rules_based;
#[cfg(feature = "exposure-events")]
pub mod telemetry;

pub use flag_type::{ExpectedFlagType, FlagType};
