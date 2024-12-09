// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Extract the entity id, container id and external env
//!
//! The container id can be extracted from `/proc/self/group`
//!
//! The entity id is one of:
//! - `cid:<container id>` if available
//! - `in:<cgroup node inode>` if container id is not available (e.g. when using cgroupV2)
//!
//! The external env is an environment variable provided by the admission controller.
//!
//! # References
//! - [DataDog/dd-trace-go](https://github.com/DataDog/dd-trace-go/blob/v1/internal/container.go)
//! - [Qard/container-info](https://github.com/Qard/container-info/blob/master/index.js)
//! # Supported environments
//! ## Docker
//! /proc/self/cgroup should contain lines like:
//! ```text
//! 13:name=systemd:/docker/3726184226f5d3147c25fdeab5b60097e378e8a720503a5e19ecfdf29f869860
//! ```
//! ## Kubernetes
//! /proc/self/cgroup should contain lines like:
//! ```text
//! 11:perf_event:/kubepods/besteffort/pod3d274242-8ee0-11e9-a8a6-1e68d864ef1a/3e74d3fd9db4c9dd921ae05c2502fb984d0cde1b36e581b13f79c639da4518a1
//! ```
//!
//! Possibly with extra characters before id:
//! ```text
//! 1:name=systemd:/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod2d3da189_6407_48e3_9ab6_78188d75e609.slice/docker-7b8952daecf4c0e44bbcefe1b5c5ebc7b4839d4eefeccefe694709d3809b6199.scope
//! ```
//!
//! Or a UUID:
//! ```text
//! 1:name=systemd:/kubepods/besteffort/pode9b90526-f47d-11e8-b2a5-080027b9f4fb/15aa6e53-b09a-40c7-8558-c6c31e36c88a
//! ```
//! ## ECS
//! /proc/self/cgroup should contain lines like:
//! ```text
//! 9:perf_event:/ecs/haissam-ecs-classic/5a0d5ceddf6c44c1928d367a815d890f/38fac3e99302b3622be089dd41e7ccf38aff368a86cc339972075136ee2710ce
//! ```
//! ## Fargate 1.3-:
//! /proc/self/cgroup should contain lines like:
//! ```test
//! 11:hugetlb:/ecs/55091c13-b8cf-4801-b527-f4601742204d/432624d2150b349fe35ba397284dea788c2bf66b885d14dfc1569b01890ca7da
//! ```
//! ## Fargate 1.4+:
//! Here we match a task id with a suffix
//! ```test
//! 1:name=systemd:/ecs/8cd79a803caf4d2aa945152e934a5c00/8cd79a803caf4d2aa945152e934a5c00-1053176469
//! ```

use crate::config::parse_env;
use lazy_static::lazy_static;

const EXTERNAL_ENV_ENVIRONMENT_VARIABLE: &str = "DD_EXTERNAL_ENV";

/// Unix specific module allowing the use of unix specific functions
#[cfg(unix)]
mod unix;

/// Returns the `container_id` if available in the cgroup file, otherwise returns `None`
pub fn get_container_id() -> Option<&'static str> {
    #[cfg(unix)]
    {
        unix::get_container_id()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Returns the `entity_id` if available, either `cid-<container_id>` or `in-<cgroup_inode>`
pub fn get_entity_id() -> Option<&'static str> {
    #[cfg(unix)]
    {
        unix::get_entity_id()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Returns the `DD_EXTERNAL_ENV` if available as an env variable
pub fn get_external_env() -> Option<&'static str> {
    lazy_static! {
        static ref DD_EXTERNAL_ENV: Option<String> =
            parse_env::str_not_empty(EXTERNAL_ENV_ENVIRONMENT_VARIABLE);
    }
    DD_EXTERNAL_ENV.as_deref()
}
