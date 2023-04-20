// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

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
            Logs(_) => "logs",
            MessageBatch(_) => "message-batch",
            AppExtendedHeartbeat(_) => "app-extended-heartbeat",
        }
    }
}
