// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::SliceSet;
use datadog_profiling_protobuf::Label;

/// Holds a set of labels. Labels are not sorted--the input order does matter.
pub type LabelsSet = SliceSet<Label>;
