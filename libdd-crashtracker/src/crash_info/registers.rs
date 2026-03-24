// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const NULL_THRESHOLD: u64 = 0x10000; // 64KB — covers typical vm.mmap_min_addr

/// Parsed CPU register state from ucontext.
///
/// Architecture-specific register names are mapped to canonical fields
/// (`ip`, `sp`, `fp`) for use by the diagnosis engine. The full set of
/// general-purpose registers is available in `general`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Registers {
    /// Instruction pointer (RIP on x86_64, PC on aarch64)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<u64>,
    /// Stack pointer (RSP on x86_64, SP on aarch64)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sp: Option<u64>,
    /// Frame pointer (RBP on x86_64, X29 on aarch64)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<u64>,
    /// All general-purpose registers by name
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub general: HashMap<String, u64>,
}

impl Registers {
    /// Attempt to parse from a structured JSON ucontext string.
    ///
    /// Returns `None` if the string is not valid JSON or not in the expected
    /// format.
    pub fn from_ucontext_json(json_str: &str) -> Option<Self> {
        let raw: HashMap<String, String> = serde_json::from_str(json_str).ok()?;

        let mut general = HashMap::new();
        for (name, hex_val) in &raw {
            if let Some(val) = parse_hex_value(hex_val) {
                general.insert(name.clone(), val);
            }
        }

        if general.is_empty() {
            return None;
        }

        // Map architecture-specific names to canonical fields
        let ip = general.get("rip").or_else(|| general.get("pc")).copied();
        let sp = general.get("rsp").or_else(|| general.get("sp")).copied();
        let fp = general.get("rbp").or_else(|| general.get("x29")).copied();

        Some(Self {
            ip,
            sp,
            fp,
            general,
        })
    }

    /// Returns names of registers whose values fall below the null threshold
    /// (likely null pointer or small offset from null).
    pub fn near_null_registers(&self) -> Vec<String> {
        let mut result: Vec<String> = self
            .general
            .iter()
            .filter(|(_, &val)| val < NULL_THRESHOLD)
            .map(|(name, _)| name.clone())
            .collect();
        result.sort();
        result
    }
}

fn parse_hex_value(s: &str) -> Option<u64> {
    let stripped = s.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(stripped, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_x86_64_ucontext_json() {
        let json = r#"{
            "rip": "0x000055a3f2c01234",
            "rsp": "0x00007ffc89abcdef",
            "rbp": "0x00007ffc89abce00",
            "rax": "0x0000000000000000",
            "rbx": "0x0000000000000001",
            "rcx": "0x000055a3f2c05678",
            "rdx": "0x0000000000000042",
            "rsi": "0x00007f8a12345678",
            "rdi": "0x0000000000000018",
            "r8": "0x0000000000000000",
            "r9": "0x0000000000000000",
            "r10": "0x0000000000000000",
            "r11": "0x0000000000000000",
            "r12": "0x0000000000000000",
            "r13": "0x0000000000000000",
            "r14": "0x0000000000000000",
            "r15": "0x0000000000000000"
        }"#;

        let regs = Registers::from_ucontext_json(json).unwrap();
        assert_eq!(regs.ip, Some(0x55a3f2c01234));
        assert_eq!(regs.sp, Some(0x7ffc89abcdef));
        assert_eq!(regs.fp, Some(0x7ffc89abce00));
        assert_eq!(regs.general["rax"], 0);
        assert_eq!(regs.general["rdx"], 0x42);
    }

    #[test]
    fn test_parse_aarch64_ucontext_json() {
        let json = r#"{
            "pc": "0x0000aaaabbbb1234",
            "sp": "0x0000ffffcccc5678",
            "x29": "0x0000ffffcccc5680",
            "x0": "0x0000000000000000",
            "x1": "0x0000000000000018"
        }"#;

        let regs = Registers::from_ucontext_json(json).unwrap();
        assert_eq!(regs.ip, Some(0xaaaabbbb1234));
        assert_eq!(regs.sp, Some(0xffffcccc5678));
        assert_eq!(regs.fp, Some(0xffffcccc5680));
    }

    #[test]
    fn test_debug_format_returns_none() {
        let debug_str = "ucontext_t { uc_flags: 0, uc_link: 0x0, uc_stack: ... }";
        assert!(Registers::from_ucontext_json(debug_str).is_none());
    }

    #[test]
    fn test_empty_json_returns_none() {
        assert!(Registers::from_ucontext_json("{}").is_none());
    }

    #[test]
    fn test_invalid_json_returns_none() {
        assert!(Registers::from_ucontext_json("not json at all").is_none());
    }

    #[test]
    fn test_near_null_registers() {
        let json = r#"{
            "rip": "0x000055a3f2c01234",
            "rsp": "0x00007ffc89abcdef",
            "rbp": "0x00007ffc89abce00",
            "rax": "0x0000000000000000",
            "rdi": "0x0000000000000018",
            "rcx": "0x000055a3f2c05678"
        }"#;

        let regs = Registers::from_ucontext_json(json).unwrap();
        let null_regs = regs.near_null_registers();
        assert!(null_regs.contains(&"rax".to_string()));
        assert!(null_regs.contains(&"rdi".to_string()));
        assert!(!null_regs.contains(&"rip".to_string()));
        assert!(!null_regs.contains(&"rcx".to_string()));
    }

    #[test]
    fn test_parse_hex_value() {
        assert_eq!(parse_hex_value("0x1234"), Some(0x1234));
        assert_eq!(parse_hex_value("0X1234"), Some(0x1234));
        assert_eq!(parse_hex_value("1234"), Some(0x1234));
        assert_eq!(parse_hex_value("0x0000000000000000"), Some(0));
        assert_eq!(parse_hex_value("not_hex"), None);
    }
}
