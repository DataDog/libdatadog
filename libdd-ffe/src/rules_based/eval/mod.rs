// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod eval_assignment;
mod eval_rules;
mod evaluation_context;

pub use eval_assignment::{
    EvaluationOptions, EvaluationRecord, EvaluationResult, ExperimentationOptOutMode,
    get_assignment,
};
pub use evaluation_context::EvaluationContext;

#[cfg(test)]
mod dependent_flags_tests;
