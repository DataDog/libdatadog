// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSliceSet, SetId, ThinSlice};
use crate::profiles::datatypes::Location;

pub type StackId = ThinSlice<'static, SetId<Location>>;

pub type StackSet = ParallelSliceSet<SetId<Location>>;
