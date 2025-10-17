// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSliceSet, SetId, ThinSlice};
use crate::profiles::datatypes::Location2;

pub type StackId2 = ThinSlice<'static, SetId<Location2>>;

pub type StackSet = ParallelSliceSet<SetId<Location2>>;
