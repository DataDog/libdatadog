// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use heapless::{String as HeaplessString, Vec as HeaplessVec};
use serde::Serialize;

use super::capabilities::{Capabilities, Degradations};
use super::fmt::hex_addr;
use super::signal_names::{rust_si_code_name, rust_signal_name, signal_has_address};

pub const SECTION_BUF_CAPACITY: usize = 4096;
pub const TAG_CAPACITY: usize = 288;
pub const MAX_TAGS: usize = 20;
pub const FRAME_IP_CAPACITY: usize = 2 + core::mem::size_of::<usize>() * 2;
pub const MESSAGE_CAPACITY: usize = 192;

pub type Tag = HeaplessString<TAG_CAPACITY>;
pub type Tags = HeaplessVec<Tag, MAX_TAGS>;

#[derive(Serialize)]
pub struct Metadata<'a> {
    pub library_name: &'a str,
    pub library_version: &'a str,
    pub family: &'a str,
    pub tags: Tags,
}

impl<'a> Metadata<'a> {
    pub fn new(library_name: &'a str, library_version: &'a str, family: &'a str) -> Self {
        Self {
            library_name,
            library_version,
            family,
            tags: Tags::new(),
        }
    }
}

#[derive(Serialize)]
pub struct SignalInfo {
    pub si_signo: i32,
    pub si_code: i32,
    pub si_signo_human_readable: &'static str,
    pub si_code_human_readable: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub si_addr: Option<HeaplessString<FRAME_IP_CAPACITY>>,
}

impl SignalInfo {
    pub fn new(si_signo: i32, si_code: i32, si_addr: usize, has_siginfo: bool) -> Self {
        let si_addr = if has_siginfo && signal_has_address(si_signo) {
            Some(hex_addr(si_addr))
        } else {
            None
        };

        Self {
            si_signo,
            si_code,
            si_signo_human_readable: rust_signal_name(si_signo),
            si_code_human_readable: rust_si_code_name(si_signo, si_code),
            si_addr,
        }
    }
}

#[derive(Serialize)]
pub struct ProcInfo {
    pub pid: i32,
    pub tid: i32,
}

#[derive(Serialize)]
pub struct Frame {
    pub ip: HeaplessString<FRAME_IP_CAPACITY>,
}

impl Frame {
    pub fn from_ip(ip: usize) -> Self {
        Self { ip: hex_addr(ip) }
    }
}

pub struct CrashContext<'a> {
    pub signal: SignalInfo,
    pub pid: i32,
    pub tid: i32,
    pub frames: &'a [usize],
}

pub struct Report<'a> {
    pub config_json: &'a str,
    pub library_name: &'a str,
    pub library_version: &'a str,
    pub family: &'a str,
    pub default_service: &'a str,
    pub service: &'a str,
    pub env: &'a str,
    pub app_version: &'a str,
    pub runtime_id: &'a str,
    pub platform: &'a str,
    pub stage_name: &'a str,
    pub stackwalk_method: &'a str,
    pub capabilities: Capabilities,
    pub degradations: Degradations,
}
