// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Spawn-related types shared across platforms.
//!
//! Task spawning is handled internally by `SharedRuntime`; this module only
//! provides the executor-agnostic [`SpawnError`] type used in join handles.

use core::fmt;

/// Executor-agnostic error returned when a spawned task is aborted or panics.
#[derive(Debug)]
pub struct SpawnError {
    msg: String,
}

impl SpawnError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "spawned task failed: {}", self.msg)
    }
}

impl core::error::Error for SpawnError {}
