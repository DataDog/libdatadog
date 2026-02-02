// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Platform constants for crash tracking.
//!
//! This module provides platform-specific constants used during stack unwinding.

/// Maximum number of frames to collect in a backtrace.
///
/// This limit prevents runaway frame walking in case of stack corruption.
pub const MAX_BACKTRACE_FRAMES: usize = 128;
