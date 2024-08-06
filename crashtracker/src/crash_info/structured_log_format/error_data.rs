// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::stacktrace::{StackFrame, StackType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ErrorKind {
    SigBus,
    SigSegv,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorData {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub additional_stacks: HashMap<String, Vec<StackFrame>>,
    pub is_crash: bool,
    pub kind: ErrorKind,
    pub message: String,
    pub stack: Vec<StackFrame>,
    pub stack_type: StackType,
}
