// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

// Enum for supported signal types
#[derive(Debug, Clone, Copy, PartialEq)]
enum SignalType {
    SIGSEGV,
    SIGBUS,
    SIGILL,
    SIGFPE,
    SIGABRT,
    SIGTRAP,
    SIGSYS,
    SIGXCPU,
    SIGXFSZ,
    SIGSTKFLT,
    SIGPOLL,
    SIGPROF,
    SIGVTALRM,
    SIGIO,
    SIGPWR,
    SIGWINCH,
    SIGUNUSED,
    SIGRTMIN,
    SIGRTMAX,
}

struct FaultInfo {
    pub fault_type: String,
    pub fault_addr: u64,
    pub fault_pid: u32,
    pub fault_tid: u32,
    pub fault_time: u64,
}
