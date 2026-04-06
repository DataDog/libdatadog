// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Structured representation of the CPU register state captured from a
/// `ucontext_t` at the time of a crash signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ucontext {
    /// Target architecture: "x86_64" or "aarch64"
    pub arch: String,
    /// Named registers mapped to their hex-formatted values ("rip" -> "0x00007f...")
    pub registers: HashMap<String, String>,
    /// Full Debug-formatted ucontext string preserving FPU state, signal mask,
    /// and alternate-stack info that is not captured in `registers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

#[cfg(test)]
impl Ucontext {
    pub fn test_instance(_seed: u64) -> Self {
        Self {
            arch: "x86_64".to_string(),
            registers: HashMap::from([
                ("rip".to_string(), "0x00007f7e11d3a2b0".to_string()),
                ("rsp".to_string(), "0x00007f7e11d3a2b0".to_string()),
                ("rbp".to_string(), "0x00007f7e11d3a2b0".to_string()),
                ("rax".to_string(), "0x00007f7e11d3a2b0".to_string()),
                ("rbx".to_string(), "0x00007f7e11d3a2b0".to_string()),
                ("rcx".to_string(), "0x00007f7e11d3a2b0".to_string()),
            ]),
            raw: Some("ucontext_t { uc_flags: 7, uc_link: 0x0, uc_stack: stack_t { ss_sp: 0x713fa26f1000, ss_flags: 0, ss_size: 65536 } }".to_string()),
        }
    }
}
