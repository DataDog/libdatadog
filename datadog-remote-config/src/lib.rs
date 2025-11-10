// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod config;
pub mod fetch;
pub mod file_change_tracker;
pub mod file_storage;
mod parse;
mod path;
mod targets;

use libdd_common::tag::Tag;
pub use parse::*;
pub use path::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, Hash, Ord, PartialOrd, Eq, PartialEq)]
pub struct Target {
    pub service: String,
    pub env: String,
    pub app_version: String,
    pub tags: Vec<Tag>,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteConfigCapabilities {
    AsmActivation = 1,
    AsmIpBlocking = 2,
    AsmDdRules = 3,
    AsmExclusions = 4,
    AsmRequestBlocking = 5,
    AsmResponseBlocking = 6,
    AsmUserBlocking = 7,
    AsmCustomRules = 8,
    AsmCustomBlockingResponse = 9,
    AsmTrustedIps = 10,
    AsmApiSecuritySampleRate = 11,
    ApmTracingSampleRate = 12,
    ApmTracingLogsInjection = 13,
    ApmTracingHttpHeaderTags = 14,
    ApmTracingCustomTags = 15,
    AsmProcessorOverrides = 16,
    AsmCustomDataScanners = 17,
    AsmExclusionData = 18,
    ApmTracingEnabled = 19,
    ApmTracingDataStreamsEnabled = 20,
    AsmRaspSqli = 21,
    AsmRaspLfi = 22,
    AsmRaspSsrf = 23,
    AsmRaspShi = 24,
    AsmRaspXxe = 25,
    AsmRaspRce = 26,
    AsmRaspNosqli = 27,
    AsmRaspXss = 28,
    ApmTracingSampleRules = 29,
    CsmActivation = 30,
    AsmAutoUserInstrumMode = 31,
    AsmEndpointFingerprint = 32,
    AsmSessionFingerprint = 33,
    AsmNetworkFingerprint = 34,
    AsmHeaderFingerprint = 35,
    AsmTruncationRules = 36,
    AsmRaspCmdi = 37,
    ApmTracingEnableDynamicInstrumentation = 38,
    ApmTracingEnableExceptionReplay = 39,
    ApmTracingEnableCodeOrigin = 40,
    ApmTracingEnableLiveDebugging = 41,
    AsmDdMulticonfig = 42,
    AsmTraceTaggingRules = 43,
}
