// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared EVP proxy constants for sidecar services.

/// EVP subdomain header name.
pub(crate) const SUBDOMAIN_HEADER: &str = "X-Datadog-EVP-Subdomain";

/// EVP subdomain that routes requests to event-platform intake.
pub(crate) const EVENT_PLATFORM_INTAKE_SUBDOMAIN: &str = "event-platform-intake";

/// EVP uncompressed request-body limit.
pub(crate) const PAYLOAD_SIZE_LIMIT: usize = 5 * 1024 * 1024;
