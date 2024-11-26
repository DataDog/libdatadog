// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod parse_env {
    use std::{env, str::FromStr, time::Duration};

    pub fn duration(name: &str) -> Option<Duration> {
        Some(Duration::from_secs_f32(
            env::var(name).ok()?.parse::<f32>().ok()?,
        ))
    }

    pub fn int<T: FromStr>(name: &str) -> Option<T> {
        env::var(name).ok()?.parse::<T>().ok()
    }

    pub fn bool(name: &str) -> Option<bool> {
        let var = env::var(name).ok()?;
        Some(var == "true" || var == "1")
    }

    pub fn str_not_empty(name: &str) -> Option<String> {
        env::var(name).ok().filter(|s| !s.is_empty())
    }
}
