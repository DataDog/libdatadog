// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StackType {
    CrashTrackerV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackTrace {
    pub format: StackType,
    pub trace: Vec<StackFrame>,
}

/// All fields are hex encoded integers.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_base_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_ip: Option<NormalizedAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NormalizedAddressMeta {
    Apk(PathBuf),
    Elf {
        path: PathBuf,
        build_id: Option<Vec<u8>>,
    },
    Unknown,
    Unexpected(String),
}

impl From<crate::crash_info::internal::NormalizedAddressMeta> for NormalizedAddressMeta {
    fn from(value: crate::crash_info::internal::NormalizedAddressMeta) -> Self {
        match value {
            crate::NormalizedAddressMeta::Apk(path) => NormalizedAddressMeta::Apk(path),
            crate::NormalizedAddressMeta::Elf { path, build_id } => {
                NormalizedAddressMeta::Elf { path, build_id }
            }
            crate::NormalizedAddressMeta::Unknown => NormalizedAddressMeta::Unknown,
            crate::NormalizedAddressMeta::Unexpected(msg) => NormalizedAddressMeta::Unexpected(msg),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedAddress {
    pub file_offset: u64,
    pub meta: NormalizedAddressMeta,
}

impl From<crate::crash_info::internal::NormalizedAddress> for NormalizedAddress {
    fn from(value: crate::crash_info::internal::NormalizedAddress) -> Self {
        Self {
            file_offset: value.file_offset,
            meta: value.meta.into(),
        }
    }
}

impl From<Vec<crate::crash_info::internal::StackFrame>> for StackTrace {
    fn from(value: Vec<crate::crash_info::internal::StackFrame>) -> Self {
        let mut trace = vec![];
        for frame in value {
            if frame.names.is_empty() {
                trace.push(StackFrame {
                    column: None,
                    file: None,
                    function: None,
                    ip: frame.ip,
                    line: None,
                    module_base_address: frame.module_base_address,
                    normalized_ip: frame.normalized_ip.map(NormalizedAddress::from),
                    sp: frame.sp,
                    symbol_address: frame.symbol_address,
                })
            } else {
                for name in frame.names {
                    trace.push(StackFrame {
                        column: name.colno,
                        file: name.filename,
                        function: name.name,
                        ip: frame.ip.clone(),
                        line: name.lineno,
                        module_base_address: frame.module_base_address.clone(),
                        normalized_ip: frame.normalized_ip.clone().map(NormalizedAddress::from),
                        sp: frame.sp.clone(),
                        symbol_address: frame.symbol_address.clone(),
                    })
                }
            }
        }
        Self {
            trace,
            format: StackType::CrashTrackerV1,
        }
    }
}
