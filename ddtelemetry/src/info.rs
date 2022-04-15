// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub mod os {
    // TODO: this function will call API's (fargate, k8s, etc) in the future to get to real host API
    pub async fn real_hostname() -> anyhow::Result<String> {
        Ok(sys_info::hostname()?)
    }

    pub const fn os_name() -> &'static str {
        std::env::consts::OS
    }

    pub fn os_version() -> anyhow::Result<String> {
        sys_info::os_release().map_err(|e| e.into())
    }
}
