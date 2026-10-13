// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod attributes;
mod configuration;
mod error;
mod eval;
mod sharder;
mod str;
mod timestamp;
mod ufc;

pub use attributes::Attribute;
pub use configuration::Configuration;
pub use error::EvaluationError;
pub use eval::{EvaluationContext, get_assignment};
pub use str::Str;
pub use timestamp::{Timestamp, now};
pub use ufc::{Assignment, AssignmentReason, AssignmentValue, UniversalFlagConfig};

pub use crate::{ExpectedFlagType, FlagType};
