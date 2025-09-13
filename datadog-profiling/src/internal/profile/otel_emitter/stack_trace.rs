// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::Id;
use crate::internal::StackTrace as InternalStackTrace;

// For owned values - forward to reference version
impl From<InternalStackTrace> for datadog_profiling_otel::Stack {
    fn from(internal_stack_trace: InternalStackTrace) -> Self {
        Self::from(&internal_stack_trace)
    }
}

// For references (existing implementation)
impl From<&InternalStackTrace> for datadog_profiling_otel::Stack {
    fn from(internal_stack_trace: &InternalStackTrace) -> Self {
        Self {
            location_indices: internal_stack_trace
                .locations
                .iter()
                .map(|location_id| location_id.to_raw_id() as i32)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::LocationId;

    #[test]
    fn test_from_internal_stack_trace() {
        // Test with locations
        let internal_stack_trace = InternalStackTrace {
            locations: vec![
                LocationId::from_offset(0),
                LocationId::from_offset(1),
                LocationId::from_offset(2),
            ],
        };

        let otel_stack = datadog_profiling_otel::Stack::from(&internal_stack_trace);
        assert_eq!(otel_stack.location_indices, vec![1, 2, 3]);

        // Test with empty locations
        let internal_stack_trace = InternalStackTrace { locations: vec![] };

        let otel_stack = datadog_profiling_otel::Stack::from(&internal_stack_trace);
        assert_eq!(otel_stack.location_indices, vec![] as Vec<i32>);
    }
}
