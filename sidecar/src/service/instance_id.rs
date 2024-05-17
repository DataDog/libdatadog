// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

/// `InstanceId` is a structure that holds session and runtime identifiers.
#[derive(Default, Clone, Hash, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstanceId {
    pub session_id: String,
    pub runtime_id: String,
}

impl InstanceId {
    /// Creates a new `InstanceId` with the given session and runtime identifiers.
    ///
    /// # Arguments
    ///
    /// * `session_id` - An entity that can be converted to a String that holds the session
    ///   identifier.
    /// * `runtime_id` - A entity that can be converted to a String that holds the runtime
    ///   identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_sidecar::service::InstanceId;
    /// let instance_id = InstanceId::new("test_session", "test_runtime");
    /// ```
    pub fn new<T>(session_id: T, runtime_id: T) -> Self
    where
        T: Into<String>,
    {
        InstanceId {
            session_id: session_id.into(),
            runtime_id: runtime_id.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_id_new() {
        let session_id = "test_session";
        let runtime_id = "test_runtime";

        let instance_id = InstanceId::new(session_id, runtime_id);

        assert_eq!(instance_id.session_id, session_id);
        assert_eq!(instance_id.runtime_id, runtime_id);
    }
}
