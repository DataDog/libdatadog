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
    AppHearbeat(#[serde(skip_serializing)] ()),
    AppClosing(#[serde(skip_serializing)] ()),
    GenerateMetrics(GenerateMetrics),
    Logs(Vec<Log>),
    MessageBatch(Vec<Payload>),
    AppExtendedHeartbeats(AppStarted),
}
