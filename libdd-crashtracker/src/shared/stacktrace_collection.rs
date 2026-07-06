// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

/// Stacktrace collection occurs in the context of a crashing process.
/// If the stack is sufficiently corrupted, stacktrace collection itself may fail.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "std", derive(schemars::JsonSchema))]
pub enum StacktraceCollection {
    #[default]
    Disabled,
    WithoutSymbols,
    /// Resolve symbols in the crashing process.
    EnabledWithInprocessSymbols,
    EnabledWithSymbolsInReceiver,
}
