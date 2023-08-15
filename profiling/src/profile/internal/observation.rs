// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

/// This represents the set of values observed for a sample
/// Since the length is fixed for a given Profile, no need
/// to store the `capacity` field. Saves 8 bytes vs `Vec<i64>`.
#[repr(transparent)]
pub struct Observation {
    data: Box<[i64]>,
}

impl From<Vec<i64>> for Observation {
    fn from(v: Vec<i64>) -> Self {
        let data = v.into_boxed_slice();
        Self { data }
    }
}

impl AsRef<[i64]> for Observation {
    fn as_ref(&self) -> &[i64] {
        &self.data
    }
}

impl Observation {
    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, i64> {
        self.data.iter_mut()
    }
}
