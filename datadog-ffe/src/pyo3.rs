// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use pyo3::prelude::*;

use crate::rules_based::{Assignment, AssignmentReason};

/// Initialize FFE Python classes under the given module.
pub fn init(m: &Bound<PyModule>) -> PyResult<()> {
    m.add_class::<AssignmentReason>()?;
    m.add_class::<Assignment>()?;

    Ok(())
}
