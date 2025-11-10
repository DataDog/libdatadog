// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::mutex_atomic)]
#![allow(clippy::nonminimal_bool)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use libdd_common::entity_id;
use tracing::debug;

pub mod config;
pub mod data;
pub mod info;
pub mod metrics;
pub mod worker;

pub fn build_host() -> data::Host {
    debug!("Building telemetry host information");
    let hostname = info::os::real_hostname().unwrap_or_else(|_| String::from("unknown_hostname"));
    let container_id = entity_id::get_container_id().map(|f| f.to_string());
    let os_version = info::os::os_version().ok();

    debug!(
        host.hostname = %hostname,
        host.container_id = ?container_id,
        host.os = info::os::os_name(),
        host.os_version = ?os_version,
        "Built telemetry host information"
    );

    data::Host {
        hostname,
        container_id,
        os: Some(String::from(info::os::os_name())),
        os_version,
        kernel_name: info::os::os_type(),
        kernel_release: info::os::os_release(),
        #[cfg(unix)]
        kernel_version: unsafe { info::os::uname() },
        #[cfg(windows)]
        kernel_version: winver::WindowsVersion::detect()
            .map(|wv| format!("{}.{}.{}", wv.major, wv.minor, wv.build)),
        #[cfg(not(any(windows, unix)))]
        kernel_version: None,
    }
}
