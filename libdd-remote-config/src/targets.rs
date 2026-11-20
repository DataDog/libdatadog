// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::HashMap;
use std::str::FromStr;

// Note that this file has some #[allow(dead_code)] annotations, which cover fields which are not
// actually used, but needed for proper serialization / alignment with the remote config spec.

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
pub struct TargetsList<'a> {
    #[serde(borrow)]
    pub signed: TargetsData<'a>,
}

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
pub struct TargetsData<'a> {
    #[serde(borrow)]
    pub custom: TargetsCustom<'a>,
    #[serde(borrow)]
    pub targets: HashMap<&'a str, TargetData<'a>>,
    pub version: i64,
}

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
#[allow(dead_code)]
pub struct TargetsCustom<'a> {
    pub agent_refresh_interval: Option<u64>,
    pub opaque_backend_state: &'a str,
}

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
#[allow(dead_code)]
pub struct TargetData<'a> {
    #[serde(borrow)]
    pub custom: HashMap<&'a str, &'a RawValue>,
    pub hashes: HashMap<&'a str, &'a str>,
    pub length: u32,
}

impl<'a> TargetsList<'a> {
    pub fn try_parse(data: &'a [u8]) -> serde_json::error::Result<Self> {
        serde_json::from_slice(data)
    }
}

impl TargetData<'_> {
    pub fn try_parse_version(&self) -> Option<u64> {
        self.custom
            .get("v")
            .and_then(|v| u64::from_str(v.get()).ok())
    }
}

#[derive(Deserialize)]
pub struct RootSigned {
    pub version: u64,
}

#[derive(Deserialize)]
pub struct Root {
    pub signed: RootSigned,
}

impl Root {
    pub fn try_parse(data: &[u8]) -> serde_json::error::Result<Self> {
        serde_json::from_slice(data)
    }
}
