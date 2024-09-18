// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use crate::primary_sidecar_identifier;

pub fn crashtracker_unix_socket_path() -> PathBuf {
    let base_path = format!(
        concat!("libdatadog.ct.", crate::sidecar_version!(), "@{}.sock"),
        primary_sidecar_identifier()
    );
    #[cfg(target_os = "linux")]
    let ret = base_path.into();
    #[cfg(not(target_os = "linux"))]
    let ret = std::env::temp_dir().join(base_path);
    ret
}
