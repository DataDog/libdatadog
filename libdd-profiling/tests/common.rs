// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Common test utilities

use std::collections::HashMap;

/// Validates that entity headers (container-id, entity-id, external-env) match
/// the values provided by libdd_common::entity_id
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
