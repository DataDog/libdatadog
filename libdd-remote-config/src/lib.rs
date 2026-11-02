// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod config;
#[cfg(feature = "client")]
pub mod fetch;
#[cfg(feature = "client")]
pub mod file_change_tracker;
#[cfg(feature = "client")]
pub mod file_storage;
pub mod parse;
mod path;
#[cfg(feature = "client")]
mod targets;

pub use parse::*;
pub use path::*;

use serde::{Deserialize, Serialize};

/// An unsigned, big-endian remote-config capability mask.
///
/// Bit zero is the least significant bit of the final byte. Empty masks and leading
/// zero bytes are valid. Single clients preserve the bytes unchanged. Shared
/// clients advertise the numeric union of their runtimes' masks.
///
/// Consumers own capability assignments. Unknown bits and arbitrary widths are
/// accepted without consulting [`RemoteConfigCapabilities`].
///
/// Enum constructors and setters remain available during migration. Client callers
/// opt in with `fetch::SingleFetcher::new_with_capability_mask` or
/// `fetch::MultiTargetFetcher::add_runtime_with_capability_mask`.
#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityMask(Vec<u8>);

impl From<Vec<u8>> for CapabilityMask {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for CapabilityMask {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl FromIterator<RemoteConfigCapabilities> for CapabilityMask {
    fn from_iter<T: IntoIterator<Item = RemoteConfigCapabilities>>(capabilities: T) -> Self {
        Self::from_bit_positions(
            capabilities
                .into_iter()
                .map(|capability| capability as usize),
        )
    }
}

impl CapabilityMask {
    #[cfg(all(feature = "client", not(target_arch = "wasm32")))]
    pub(crate) fn set_bits(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().rev().enumerate().flat_map(|(offset, byte)| {
            (0..8).filter_map(move |bit| (byte & (1 << bit) != 0).then_some(offset * 8 + bit))
        })
    }

    pub(crate) fn from_bit_positions(positions: impl Iterator<Item = usize>) -> Self {
        let mut bytes = Vec::new();
        for position in positions {
            let offset = position / 8;
            if bytes.len() <= offset {
                bytes.resize(offset + 1, 0);
            }
            bytes[offset] |= 1 << (position % 8);
        }
        bytes.reverse();
        Self(bytes)
    }
}

#[cfg(all(test, feature = "client", not(target_arch = "wasm32")))]
mod capability_mask_tests {
    use super::CapabilityMask;

    #[test]
    fn bit_positions_cross_integer_and_byte_boundaries() {
        let mut bytes = vec![0; 33];
        bytes[0] = 1;
        bytes[1] = 0x80;
        bytes[16] = 1;
        bytes[17] = 0x80;
        bytes[24] = 1;
        bytes[25] = 0x80;
        bytes[26] = 0x60;
        bytes[28] = 1;
        bytes[29] = 0x80;
        bytes[31] = 1;
        bytes[32] = 0x81;
        let mask = CapabilityMask::from(bytes.clone());
        assert_eq!(
            mask.set_bits().collect::<Vec<_>>(),
            [0, 7, 8, 31, 32, 53, 54, 63, 64, 127, 128, 255, 256]
        );
        assert_eq!(
            CapabilityMask::from_bit_positions(mask.set_bits()).as_ref(),
            bytes
        );
    }

    #[test]
    fn serialization_preserves_bytes_instead_of_enum_ordinals() {
        let mask = CapabilityMask::from(vec![0, 0x80, 0]);
        assert_eq!(serde_json::to_string(&mask).unwrap(), "[0,128,0]");
        assert_eq!(
            serde_json::from_str::<CapabilityMask>("[0,128,0]")
                .unwrap()
                .as_ref(),
            &[0, 0x80, 0]
        );
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Hash, Ord, PartialOrd, Eq, PartialEq)]
pub struct Target {
    service: String,
    env: String,
    app_version: String,
    tags: Vec<String>,
    process_tags: Vec<String>,
}

impl Target {
    /// Creates a new `Target`. `tags` and `process_tags` are expected as
    /// already-formatted `"key:value"` strings.
    pub fn new(
        service: String,
        env: String,
        app_version: String,
        tags: Vec<String>,
        process_tags: Vec<String>,
    ) -> Target {
        Target {
            service: service.to_string(),
            env: env.to_string(),
            app_version: app_version.to_string(),
            tags,
            process_tags,
        }
    }

    pub fn service(&self) -> &str {
        self.service.as_str()
    }

    pub fn env(&self) -> &str {
        self.env.as_str()
    }

    pub fn app_version(&self) -> &str {
        self.app_version.as_str()
    }
}

#[repr(C)]
#[derive(
    Debug,
    Copy,
    Clone,
    Hash,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    strum_macros::EnumIter,
    strum_macros::IntoStaticStr,
    strum_macros::Display,
)]
#[non_exhaustive]
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
    AsmExtendedDataCollection = 44,
    ApmTracingMulticonfig = 45,
    FfeFlagConfigurationRules = 46,
    DdDataStreamsTransactionExtractors = 47,
    LlmObsActivation = 48,
    AsmRawResponseBody = 49,
}
