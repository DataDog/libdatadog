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
    // On macOS, temp_dir() expands to a long per-session path that can exceed the 103-byte sun_path
    // limit. /tmp is always short (≤4 bytes) and guaranteed to exist.
    #[cfg(target_os = "macos")]
    let ret = std::path::Path::new("/tmp").join(base_path);
    #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
    let ret = std::env::temp_dir().join(base_path);
    ret
}
