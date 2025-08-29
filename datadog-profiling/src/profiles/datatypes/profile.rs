// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::datatypes::{Sample, ValueType};
use crate::profiles::ProfileError;
use arrayvec::ArrayVec;

pub const MAX_SAMPLE_TYPES: usize = 2;

#[derive(Debug, Default)]
pub struct Profile {
    pub sample_type: ArrayVec<ValueType, MAX_SAMPLE_TYPES>,
    pub samples: ArrayVec<Sample, MAX_SAMPLE_TYPES>,
    pub period_types: Option<ValueType>,
    pub period: Option<i64>,
}

impl Profile {
    pub fn add_sample_type(&mut self, vt: ValueType) -> Result<(), ProfileError> {
        Ok(self.sample_type.try_push(vt)?)
    }

    pub fn add_period(&mut self, period: i64, vt: ValueType) {
        self.period_types = Some(vt);
        self.period = Some(period);
    }

    pub fn add_sample(&mut self, sample: Sample) -> Result<(), ProfileError> {
        self.samples.try_push(sample)?;
        Ok(())
    }
}
