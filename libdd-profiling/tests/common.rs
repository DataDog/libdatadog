// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Common test utilities

use std::collections::HashMap;

/// Validates that entity headers (container-id, entity-id, external-env) match
/// the values provided by libdd_common::entity_id
///
/// # Current Limitations
///
/// **NOTE:** This test helper has known limitations that should be addressed in a follow-up PR:
///
/// 1. **Environment-dependent behavior**: The test changes its behavior dynamically based on the
///    exact execution environment of the test runner (e.g., whether running in a container, whether
///    certain environment variables are set).
///
/// 2. **Non-deterministic across environments**: What passes on a local machine may fail in CI (or
///    vice versa) because the underlying entity detection functions return different values in
///    different environments.
///
/// 3. **Incomplete test coverage**: We only exercise the codepaths that happen to be triggered in
///    the current test environment, not all possible combinations of entity headers being
///    present/absent.
///
/// **Future improvement**: The ideal approach would be to refactor the underlying code
/// (`libdd_common::entity_id::get_container_id()`, `get_entity_id()`, etc.) to be more testable,
/// perhaps by making them accept injectable dependencies or configuration. Then we could test all
/// combinations: container-id [Some/None] × entity-id [Some/None] × external-env [Some/None] to
/// verify correct header inclusion/exclusion in all 8 cases.
///
/// See discussion: https://github.com/DataDog/libdatadog/pull/1493#discussion_r2745712029
pub fn assert_entity_headers_match(headers: &HashMap<String, String>) {
    // Check for entity headers and validate their values match what libdd_common provides
    let expected_container_id = libdd_common::entity_id::get_container_id();
    let expected_entity_id = libdd_common::entity_id::get_entity_id();
    let expected_external_env = *libdd_common::entity_id::DD_EXTERNAL_ENV;

    // Validate container ID
    if let Some(expected) = expected_container_id {
        assert_eq!(
            headers.get("datadog-container-id"),
            Some(&expected.to_string()),
            "datadog-container-id header should match the value from entity_id::get_container_id()"
        );
    } else {
        assert!(
            !headers.contains_key("datadog-container-id"),
            "datadog-container-id header should not be present when entity_id::get_container_id() is None"
        );
    }

    // Validate entity ID
    if let Some(expected) = expected_entity_id {
        assert_eq!(
            headers.get("datadog-entity-id"),
            Some(&expected.to_string()),
            "datadog-entity-id header should match the value from entity_id::get_entity_id()"
        );
    } else {
        assert!(
            !headers.contains_key("datadog-entity-id"),
            "datadog-entity-id header should not be present when entity_id::get_entity_id() is None"
        );
    }

    // Validate external env
    if let Some(expected) = expected_external_env {
        assert_eq!(
            headers.get("datadog-external-env"),
            Some(&expected.to_string()),
            "datadog-external-env header should match the value from entity_id::DD_EXTERNAL_ENV"
        );
    } else {
        assert!(
            !headers.contains_key("datadog-external-env"),
            "datadog-external-env header should not be present when entity_id::DD_EXTERNAL_ENV is None"
        );
    }
}
