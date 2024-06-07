// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub fn set_cgroup_file(_file: String) {}

pub fn set_cgroup_mount_path(_path: String) {}

pub fn get_container_id() -> Option<&'static str> {
    None
}

pub fn get_entity_id() -> Option<&'static str> {
    None
}
