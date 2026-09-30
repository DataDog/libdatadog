// Copyright 2026 Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use datadog_sidecar::service::telemetry::TelemetryCachedClient;
use std::os::fd::AsRawFd;

#[tokio::test]
#[cfg_attr(miri, ignore)] // Requires native /dev/fd aliases.
async fn composer_paths_are_checked_before_reusing_cached_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("installed.json");
    std::fs::write(
        &path,
        r#"{"packages":[{"name":"test/package","version":"1.2.3"}]}"#,
    )
    .unwrap();

    let packages = TelemetryCachedClient::extract_composer_telemetry(path.clone()).await;
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].name, "test/package");
    assert_eq!(packages[0].version.as_deref(), Some("1.2.3"));
    assert!(
        TelemetryCachedClient::extract_composer_telemetry(dir.path().join("missing"))
            .await
            .is_empty()
    );

    let held = std::fs::File::open(&path).unwrap();
    let alias = std::path::PathBuf::from(format!("/dev/fd/{}", held.as_raw_fd()));
    assert_eq!(
        TelemetryCachedClient::extract_composer_telemetry(alias.clone())
            .await
            .len(),
        1
    );

    libdd_common::unix_utils::set_restrict_worker_file_outputs(true);
    assert_eq!(
        TelemetryCachedClient::extract_composer_telemetry(path.clone())
            .await
            .len(),
        1
    );
    #[cfg(target_os = "linux")]
    assert!(
        TelemetryCachedClient::extract_composer_telemetry(alias.clone())
            .await
            .is_empty()
    );

    // On macOS descriptor aliases must reopen the canonical path. An unlinked file cannot pass.
    std::fs::remove_file(path).unwrap();
    assert!(
        TelemetryCachedClient::extract_composer_telemetry(alias)
            .await
            .is_empty()
    );
}
