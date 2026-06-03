// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::probe_defs::LiveDebuggingData;
use datadog_remote_config::{ParseError, RemoteConfigContent, RemoteConfigProduct};

impl RemoteConfigContent for LiveDebuggingData {
    const PRODUCT: RemoteConfigProduct = RemoteConfigProduct::LiveDebugger;

    fn parse(data: &[u8]) -> Result<Self, ParseError> {
        crate::parse_json::parse(&String::from_utf8_lossy(data))
            .map_err(|e| ParseError::Custom(e.into()))
    }
}
