// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::runtime_callback::RuntimeStack;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::unknown_value::UnknownValue;

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Experimental {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ucontext: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_stack: Option<RuntimeStack>,
}

impl Experimental {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_additional_tags(mut self, additional_tags: Vec<String>) -> Self {
        self.additional_tags = additional_tags;
        self
    }

    pub fn with_ucontext(mut self, ucontext: String) -> Self {
        self.ucontext = Some(ucontext);
        self
    }

    pub fn with_runtime_stack(mut self, runtime_stack: RuntimeStack) -> Self {
        self.runtime_stack = Some(runtime_stack);
        self
    }
}

impl UnknownValue for Experimental {
    fn unknown_value() -> Self {
        Self {
            additional_tags: vec![],
            ucontext: None,
            runtime_stack: None,
        }
    }
}
