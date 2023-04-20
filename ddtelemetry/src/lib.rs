// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![allow(clippy::mutex_atomic)]
#![allow(clippy::nonminimal_bool)]

use ddcommon::container_id;

use self::config::Config;
pub mod config;
pub mod data;
pub mod info;
// For now the IPC interface only works on unix systems
#[cfg(not(windows))]
pub mod ipc;
pub mod metrics;
pub mod worker;

pub fn build_host() -> data::Host {
    data::Host {
        hostname: info::os::real_hostname().unwrap_or_else(|_| String::from("unknown_hostname")),
        container_id: container_id::get_container_id().map(|f| f.to_string()),
        os: Some(String::from(info::os::os_name())),
        os_version: info::os::os_version().ok(),
        kernel_name: None,
        kernel_release: None,
        kernel_version: None,
    }
}
