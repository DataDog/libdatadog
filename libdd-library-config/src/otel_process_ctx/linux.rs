// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::CStr;

#[cfg(feature = "process-context-reader")]
pub mod self_reader {
    pub use crate::otel_process_ctx::ProcessContextSelfReader;
}

// Re-exported for backwards compatibility.
#[cfg(feature = "process-context-writer")]
#[deprecated(note = "use libdd_library_config::otel_process_ctx directly")]
pub use super::{publish, unpublish};
#[deprecated(note = "use libdd_library_config::otel_process_ctx directly")]
pub use super::{PROCESS_CTX_VERSION, SIGNATURE};
#[cfg(feature = "process-context-reader")]
#[deprecated(note = "use libdd_library_config::otel_process_ctx::ProcessContextSelfReader")]
pub use self_reader::ProcessContextSelfReader;

/// The discoverable name of the memory mapping.
pub const MAPPING_NAME: &CStr = c"OTEL_CTX";
