// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::{Label, StackTraceId};
use std::hash::Hash;
#[derive(Eq, PartialEq, Hash)]
pub struct Sample {
    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    pub labels: Vec<Label>,

    /// Offset into `labels` for the label with key == "local root span id".
    pub local_root_span_id_label_offset: Option<usize>,

    pub stacktrace: StackTraceId,
}
