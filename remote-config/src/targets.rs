use std::collections::HashMap;
use std::str::FromStr;
use serde::Deserialize;
use serde_json::value::RawValue;
use time::OffsetDateTime;

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
pub struct TargetsList<'a> {
    #[serde(borrow)]
    pub signatures: Vec<TargetsSignature<'a>>,
    pub signed: TargetsData<'a>,
}

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
pub struct TargetsSignature<'a> {
    pub keyid: &'a str,
    pub sig: &'a str,
}

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
pub struct TargetsData<'a> {
    pub _type: &'a str,
    pub custom: TargetsCustom<'a>,
    #[serde(with = "time::serde::iso8601")]
    pub expires: OffsetDateTime,
    pub spec_version : &'a str,
    pub targets: HashMap<&'a str, TargetData<'a>>,
    pub version: i64,
}

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
pub struct TargetsCustom<'a> {
    pub agent_refresh_interval: Option<u64>,
    pub opaque_backend_state: &'a str,
}

#[derive(Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(serde::Serialize))]
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

impl<'a> TargetData<'a> {
    pub fn try_parse_version(&self) -> Option<u64> {
        self.custom.get("v").and_then(|v| u64::from_str(v.get()).ok())
    }
}