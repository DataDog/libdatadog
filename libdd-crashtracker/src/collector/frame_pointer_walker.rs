// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Stack frame representation for crash tracking.
//!
//! This module provides the `RawFrame` type used to represent a single stack
//! frame during unwinding. This is a minimal representation containing only
//! the register values, suitable for async-signal-safe contexts.

/// A raw stack frame containing register values.
///
/// This represents a single frame in the call stack, containing the
/// instruction pointer, stack pointer, and base/frame pointer at that point.
///
/// # Fields
///
/// - `ip`: Instruction pointer (RIP on x86_64, PC on aarch64)
/// - `sp`: Stack pointer (RSP on x86_64, SP on aarch64)
/// - `bp`: Base/frame pointer (RBP on x86_64, FP/X29 on aarch64)
#[derive(Debug, Clone, Copy, Default)]
pub struct RawFrame {
    /// Instruction pointer (return address for this frame)
    pub ip: usize,
    /// Stack pointer at this frame
    pub sp: usize,
    /// Base/frame pointer at this frame
    pub bp: usize,
}
