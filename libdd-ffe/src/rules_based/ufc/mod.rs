// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Universal Flag Configuration.
mod assignment;
mod compiled_flag_config;
mod models;

pub use assignment::{Assignment, AssignmentReason, AssignmentValue};
pub use compiled_flag_config::*;
pub use models::*;
