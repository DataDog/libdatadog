// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::fmt::Debug;
use std::time::SystemTime;

use chrono::{DateTime, TimeZone, Utc};

mod exporter;
mod profiles;

/// Represents time since the Unix Epoch in seconds plus nanoseconds.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Timespec {
    pub seconds: i64,
    pub nanoseconds: u32,
}

impl From<Timespec> for DateTime<Utc> {
    fn from(value: Timespec) -> Self {
        Utc.timestamp_opt(value.seconds, value.nanoseconds).unwrap()
    }
}

impl From<Timespec> for SystemTime {
    fn from(value: Timespec) -> Self {
        // The DateTime API is more convenient, so let's delegate.
        let datetime: DateTime<Utc> = value.into();
        SystemTime::from(datetime)
    }
}

impl<'a> From<&'a Timespec> for SystemTime {
    fn from(value: &'a Timespec) -> Self {
        // The DateTime API is more convenient, so let's delegate.
        let datetime: DateTime<Utc> = (*value).into();
        SystemTime::from(datetime)
    }
}

impl From<DateTime<Utc>> for Timespec {
    fn from(value: DateTime<Utc>) -> Self {
        Self {
            seconds: value.timestamp(),
            nanoseconds: value.timestamp_subsec_nanos(),
        }
    }
}

impl From<SystemTime> for Timespec {
    fn from(value: SystemTime) -> Self {
        // The DateTime API is more convenient, so let's delegate again.
        let datetime: DateTime<Utc> = value.into();
        Self::from(datetime)
    }
}
