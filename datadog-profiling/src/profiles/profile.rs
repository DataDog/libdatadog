// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ProfileId;
use crate::profiles::collections::StringOffset;
use arrayvec::ArrayVec;
// todo: ScopeProfile, ResourceProfile

// NOTE: PHP I/O profiling has I/O sample counts that it keeps but doesn't
// get displayed, they are only used for upscaling, but then we still send it
// to the backend.
//
// For instance, Socket Read Time + TimeSamples, the TimeSamples are only
// used to upscale the time.

const MAX_SAMPLE_TYPES: usize = 2;

pub struct ValueType {
    r#type: StringOffset,
    unit: StringOffset,
}

pub struct Sample {
    stack: ProfileId, // use 0 for none
    attributes: Vec<ProfileId>,
    link: ProfileId, // use 0 for none
    timestamp: i64,  // pprof is i64, Otel is u64, use 0 for none
}

pub struct Profile {
    sample_type: ArrayVec<ValueType, MAX_SAMPLE_TYPES>,
    samples: Vec<(Sample, ArrayVec<i64, MAX_SAMPLE_TYPES>)>,
    time_nanos: Option<i64>,
    duration_nanos: Option<i64>,
    period_types: Option<ValueType>,
    period: Option<i64>,
    attributes: Vec<ProfileId>,
}
