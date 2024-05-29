// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::InstanceId;

/// The `RequestIdentification` trait provides a method for extracting a request identifier.
pub(crate) trait RequestIdentification {
    /// Extracts the identifier from the request.
    ///
    /// # Returns
    ///
    /// A `RequestIdentifier` enum.
    fn extract_identifier(&self) -> RequestIdentifier;
}

/// The `RequestIdentifier` enum represents the possible identifiers for a request.
///
/// This enum is used in conjunction with the `RequestIdentification` trait to provide a flexible
/// way of identifying a request.
pub(crate) enum RequestIdentifier {
    /// Represents a request identified by an instance ID.
    InstanceId(InstanceId),
    /// Represents a request identified by a session ID.
    SessionId(String),
    /// Represents a request that is not identified.
    None,
}
