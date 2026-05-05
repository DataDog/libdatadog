// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::probe_defs::LiveDebuggingData;
use datadog_remote_config::{ProductParser, RemoteConfigParsedData, RemoteConfigProduct};
use std::any::Any;

impl RemoteConfigParsedData for LiveDebuggingData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn product(&self) -> RemoteConfigProduct {
        RemoteConfigProduct::LiveDebugger
    }
}

pub fn live_debugger_parser() -> ProductParser {
    Box::new(|data: &[u8]| {
        let s = String::from_utf8_lossy(data);
        let parsed = crate::parse_json::parse(&s)?;
        Ok(Box::new(parsed) as Box<dyn RemoteConfigParsedData>)
    })
}
