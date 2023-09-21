// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::time::SystemTime;

use super::{
    api::{self},
    internal::ValueType,
    Profile,
};

#[derive(Default)]
pub struct ProfileBuilder<'a> {
    period: Option<api::Period<'a>>,
    sample_types: Vec<api::ValueType<'a>>,
    start_time: Option<SystemTime>,
}

impl<'a> ProfileBuilder<'a> {
    pub fn build(self) -> Profile {
        let mut profile = Profile::new(self.start_time.unwrap_or_else(SystemTime::now));

        profile.sample_types = self
            .sample_types
            .iter()
            .map(|vt| ValueType {
                r#type: profile.intern(vt.r#type),
                unit: profile.intern(vt.unit),
            })
            .collect();

        if let Some(period) = self.period {
            profile.period = Some((
                period.value,
                ValueType {
                    r#type: profile.intern(period.r#type.r#type),
                    unit: profile.intern(period.r#type.unit),
                },
            ));
        };

        profile
    }

    pub const fn new() -> Self {
        ProfileBuilder {
            period: None,
            sample_types: Vec::new(),
            start_time: None,
        }
    }

    pub fn period(mut self, period: Option<api::Period<'a>>) -> Self {
        self.period = period;
        self
    }

    pub fn sample_types(mut self, sample_types: Vec<api::ValueType<'a>>) -> Self {
        self.sample_types = sample_types;
        self
    }

    pub fn start_time(mut self, start_time: Option<SystemTime>) -> Self {
        self.start_time = start_time;
        self
    }
}
