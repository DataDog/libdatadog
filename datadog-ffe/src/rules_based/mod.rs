// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod attributes;
mod configuration;
mod error;
mod eval;
mod flag_type;
mod sharder;
mod str;
mod timestamp;
mod ufc;

pub use attributes::Attribute;
pub use configuration::Configuration;
pub use error::EvaluationError;
pub use eval::{get_assignment, EvaluationContext};
pub use flag_type::{ExpectedFlagType, FlagType};
pub use str::Str;
pub use timestamp::{now, Timestamp};
pub use ufc::{Assignment, AssignmentReason, AssignmentValue, UniversalFlagConfig};
