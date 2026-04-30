// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native spawn implementation is now handled internally by `SharedRuntime`.
//!
//! This module is kept for backwards compatibility but the type is no longer
//! used by capability bundles or consumer code.

/// Marker type retained for backwards compatibility.
/// Task spawning is now handled internally by `SharedRuntime`.
#[derive(Clone, Debug)]
pub struct NativeSpawnCapability;
