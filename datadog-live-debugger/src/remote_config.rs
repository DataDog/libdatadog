// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::probe_defs::LiveDebuggingData;

impl LiveDebuggingData {
    pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
        crate::parse_json::parse(&String::from_utf8_lossy(data))
    }
}
