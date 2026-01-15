// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod config;
mod errors;
pub mod exporter_manager;
mod file_exporter;
mod profile_exporter;
#[cfg(any(test, feature = "test-utils"))]
pub mod utils;

pub use errors::SendError;
pub use exporter_manager::{ExporterManager, SuspendedExporterManager};
pub use profile_exporter::*;
