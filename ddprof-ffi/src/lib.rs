// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::convert::{TryFrom, TryInto};
use std::fmt::Debug;
use std::ops::Sub;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeZone, Utc};

mod exporter;
mod profiles;
mod slice;
mod vec;

pub use slice::{AsBytes, ByteSlice, CharSlice, Slice};
pub use vec::Vec;

/// Represents time since the Unix Epoch in seconds plus nanoseconds.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Timespec {
    pub seconds: i64,
    pub nanoseconds: u32,
}

impl From<Timespec> for DateTime<Utc> {
    fn from(value: Timespec) -> Self {
        Utc.timestamp(value.seconds, value.nanoseconds)
    }
}

impl TryFrom<SystemTime> for Timespec {
    type Error = Box<dyn std::error::Error>;

    fn try_from(value: SystemTime) -> Result<Self, Self::Error> {
        let mut duration = value.duration_since(UNIX_EPOCH)?;
        let seconds: i64 = duration.as_secs().try_into()?;
        duration = duration.sub(Duration::from_secs(seconds as u64));
        let nanoseconds: u32 = duration.as_nanos().try_into()?;
        Ok(Self {
            seconds,
            nanoseconds,
        })
    }
}
