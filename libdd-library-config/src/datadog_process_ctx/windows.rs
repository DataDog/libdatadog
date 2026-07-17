// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "process-context-reader")]
pub(crate) mod copy_pipe;
#[cfg(feature = "process-context-reader")]
pub(crate) mod reader;
#[cfg(feature = "process-context-writer")]
pub(crate) mod writer;
