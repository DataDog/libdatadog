// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling_protobuf as pprof;
use std::num::NonZeroU64;

// TODO: open draft PR for Erwan
// TODO: finish upscaling

#[derive(Clone, Copy, Debug)]
pub enum UpscalingInfo {
    Proportional {
        scale: f64,
    },
    Poisson {
        sampling_distance: NonZeroU64,
    },
    PoissonNonSampleTypeCount {
        count_value: NonZeroU64,
        sampling_distance: NonZeroU64,
    },
}

/// Labels are grouped by the (key, value), for example using strings (but of
/// course we're actually using offsets to refer to those strings):
///  - `("exception type", "OutOfBoundsException")`
///  - `("exception type", "TimeoutException")`
/// For rules which don't use label groups, use the default group `(0, 0)`.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct GroupByLabel {
    pub key: pprof::StringOffset,
    pub value: pprof::StringOffset,
}

type FxHashMap<K, V> =
    std::collections::HashMap<K, V, std::hash::BuildHasherDefault<rustc_hash::FxHasher>>;

/// A mapping between the "group by" rules to the upscaling type to apply to
/// that group. Use [`GroupByLabel::default`] to mean there isn't a label
/// grouping to apply for that group (apply to all samples of the profile
/// without additional grouping).
pub type UpscalingRules = FxHashMap<GroupByLabel, UpscalingInfo>;
