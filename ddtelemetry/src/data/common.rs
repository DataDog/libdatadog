// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

use crate::data::*;

#[derive(Serialize, Deserialize, Debug)]
pub enum ApiVersion {
    #[serde(rename = "v1")]
    V1,
    #[serde(rename = "v2")]
    V2,
}

impl ApiVersion {
    pub fn to_str(&self) -> &'static str {
        match self {
            ApiVersion::V1 => "v1",
            ApiVersion::V2 => "v2",
        }
    }
}

#[derive(Serialize, Debug)]
pub struct Telemetry<'a> {
    pub api_version: ApiVersion,
    pub tracer_time: u64,
    pub runtime_id: &'a str,
    pub seq_id: u64,
    pub application: &'a Application,
    pub host: &'a Host,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<&'a str>,
    #[serde(flatten)]
    pub payload: &'a Payload,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Application {
    pub service_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    pub language_name: String,
    pub language_version: String,
    pub tracer_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_patches: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Host {
    pub hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_version: Option<String>,
}
