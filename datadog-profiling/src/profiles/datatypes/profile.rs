// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::datatypes::{Sample, ValueType};
use crate::profiles::ProfileError;
use arrayvec::ArrayVec;
use ddcommon::vec::VecExt;

pub const MAX_SAMPLE_TYPES: usize = 2;

#[derive(Debug, Default)]
pub struct Profile {
    pub sample_type: ArrayVec<ValueType, MAX_SAMPLE_TYPES>,
    pub samples: Vec<Sample>,
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
        if self.sample_type.len() != sample.values.len() {
            return Err(self.sample_values_mismatch_error(sample.values.as_slice()));
        }
        self.samples.try_push(sample).map_err(|_| {
            ProfileError::other("out of memory: sample couldn't be added to the profile")
        })?;
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn sample_values_mismatch_error(&self, values: &[i64]) -> ProfileError {
        let sample_types = self.sample_type.len();
        let values_len = values.len();
        // todo: wire up string table so we can print out the sample type?
        ProfileError::fmt(format_args!(
            "invalid input: expected {sample_types} sample values, received {values_len}"
        ))
    }
}
