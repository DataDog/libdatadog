// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::data::*;
use serde::Serialize;

#[derive(Serialize, Debug)]
#[serde(tag = "request_type", content = "payload")]
#[serde(rename_all = "kebab-case")]
pub enum Payload {
    AppStarted(AppStarted),
    AppDependenciesLoaded(AppDependenciesLoaded),
    AppIntegrationsChange(AppIntegrationsChange),
    AppClientConfigurationChange(AppClientConfigurationChange),
    AppHeartbeat(#[serde(skip_serializing)] ()),
    AppClosing(#[serde(skip_serializing)] ()),
    GenerateMetrics(GenerateMetrics),
    Sketches(Distributions),
    Logs(Vec<Log>),
    MessageBatch(Vec<Payload>),
    AppExtendedHeartbeat(AppStarted),
}

impl Payload {
    pub fn request_type(&self) -> &'static str {
        use Payload::*;
        match self {
            AppStarted(_) => "app-started",
            AppDependenciesLoaded(_) => "app-dependencies-loaded",
            AppIntegrationsChange(_) => "app-integrations-change",
            AppClientConfigurationChange(_) => "app-client-configuration-change",
            AppHeartbeat(_) => "app-heartbeat",
            AppClosing(_) => "app-closing",
            GenerateMetrics(_) => "generate-metrics",
            Sketches(_) => "sketches",
            Logs(_) => "logs",
            MessageBatch(_) => "message-batch",
            AppExtendedHeartbeat(_) => "app-extended-heartbeat",
        }
    }
}
