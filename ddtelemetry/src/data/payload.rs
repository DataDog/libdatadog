// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::data::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "request_type", content = "payload")]
pub enum Payload {
    #[serde(rename = "app-started")]
    AppStarted(AppStarted),
    #[serde(rename = "app-dependencies-loaded")]
    AppDependenciesLoaded(AppDependenciesLoaded),
    #[serde(rename = "app-integrations-change")]
    AppIntegrationsChange(AppIntegrationsChange),
    #[serde(rename = "app-heartbeat")]
    AppHearbeat(()),
    #[serde(rename = "app-closing")]
    AppClosing(()),
    #[serde(rename = "generate-metrics")]
    GenerateMetrics(GenerateMetrics),
    #[serde(rename = "logs")]
    Logs(Vec<Log>),
}
