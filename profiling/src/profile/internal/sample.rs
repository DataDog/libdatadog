// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::{LabelId, LabelSetId, StackTraceId};
use std::hash::Hash;
#[derive(Eq, PartialEq, Hash)]
pub struct Sample {
    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    pub labels: LabelSetId,

    /// This label is stored separately, to make it easy to add the endpoint
    /// label during serialization.
    pub local_root_span_id_label: Option<LabelId>,

    pub stacktrace: StackTraceId,
}
