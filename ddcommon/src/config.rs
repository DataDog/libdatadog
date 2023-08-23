// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

pub mod parse_env {
    use http::Uri;
    use std::{env, str::FromStr, time::Duration};

    use crate::parse_uri;

    pub fn duration(name: &str) -> Option<Duration> {
        Some(Duration::from_secs_f32(
            env::var(name).ok()?.parse::<f32>().ok()?,
        ))
    }

    pub fn int<T: FromStr>(name: &str) -> Option<T> {
        env::var(name).ok()?.parse::<T>().ok()
    }

    pub fn bool(name: &str) -> Option<bool> {
        match env::var(name).ok()?.as_str() {
            "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
            _ => Some(false),
        }
    }

    pub fn str_not_empty(name: &str) -> Option<String> {
        env::var(name).ok().filter(|s| !s.is_empty())
    }

    pub fn uri(name: &str) -> Option<Uri> {
        parse_uri(&str_not_empty(name)?).ok()
    }
}
