// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::unknown_value::UnknownValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Experimental {
    pub ucontext: Option<String>,
}

impl UnknownValue for Experimental {
    fn unknown_value() -> Self {
        Self { ucontext: None }
    }
}
