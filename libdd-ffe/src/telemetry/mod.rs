// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "evaluation-metrics")]
pub mod evaluation_metrics;
#[cfg(feature = "exposure-events")]
pub mod exposures;
#[cfg(any(feature = "exposure-events", feature = "flagevaluation-evp"))]
pub mod features;
#[cfg(feature = "flagevaluation-evp")]
pub mod flagevaluation;
#[cfg(all(test, any(feature = "exposure-events", feature = "flagevaluation-evp")))]
mod test_logging;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FfeTelemetryContext {
    pub service: String,
    pub env: String,
    pub version: String,
}
