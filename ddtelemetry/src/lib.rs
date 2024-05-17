// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::mutex_atomic)]
#![allow(clippy::nonminimal_bool)]

use ddcommon::container_id;

pub mod config;
pub mod data;
pub mod info;
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
