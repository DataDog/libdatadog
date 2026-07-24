// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "evaluation-metrics")]
pub mod evaluation_metrics;
#[cfg(feature = "exposure-events")]
pub mod exposures;
#[cfg(feature = "flagevaluation-evp")]
pub mod flagevaluation;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FfeTelemetryContext {
    pub service: String,
    pub env: String,
    pub version: String,
}
