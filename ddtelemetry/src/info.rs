// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod os {
    // TODO: this function will call API's (fargate, k8s, etc) in the future to get to real host API
    pub fn real_hostname() -> anyhow::Result<String> {
        Ok(sys_info::hostname()?)
    }

    pub const fn os_name() -> &'static str {
        std::env::consts::OS
    }

    pub fn os_version() -> anyhow::Result<String> {
        sys_info::os_release().map_err(|e| e.into())
    }
}
