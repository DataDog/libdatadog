// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;

pub trait Module {
    fn install(&self) -> Result<()>;
}
