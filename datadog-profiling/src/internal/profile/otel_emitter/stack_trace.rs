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
        // Create an internal stack trace
        let internal_stack_trace = InternalStackTrace {
            locations: vec![
                LocationId::from_offset(0),
                LocationId::from_offset(1),
                LocationId::from_offset(2),
            ],
        };

        // Convert to OpenTelemetry Stack
        let otel_stack = datadog_profiling_otel::Stack::from(&internal_stack_trace);

        // Verify the conversion - note: from_offset adds 1 to avoid zero values
        assert_eq!(otel_stack.location_indices, vec![1, 2, 3]);
    }

    #[test]
    fn test_from_empty_stack_trace() {
        // Create an internal stack trace with no locations
        let internal_stack_trace = InternalStackTrace { locations: vec![] };

        // Convert to OpenTelemetry Stack
        let otel_stack = datadog_profiling_otel::Stack::from(&internal_stack_trace);

        // Verify the conversion
        assert_eq!(otel_stack.location_indices, vec![] as Vec<i32>);
    }

    #[test]
    fn test_into_otel_stack() {
        // Create an internal stack trace
        let internal_stack_trace = InternalStackTrace {
            locations: vec![
                LocationId::from_offset(10),
                LocationId::from_offset(20),
                LocationId::from_offset(30),
            ],
        };

        // Convert using .into() method
        let otel_stack: datadog_profiling_otel::Stack = (&internal_stack_trace).into();

        // Verify the conversion - note: from_offset adds 1 to avoid zero values
        assert_eq!(otel_stack.location_indices, vec![11, 21, 31]);
    }

    #[test]
    fn test_into_otel_stack_owned() {
        // Create an internal stack trace
        let internal_stack_trace = InternalStackTrace {
            locations: vec![
                LocationId::from_offset(40),
                LocationId::from_offset(50),
                LocationId::from_offset(60),
            ],
        };

        // Convert using .into() method with owned value
        let otel_stack: datadog_profiling_otel::Stack = internal_stack_trace.into();

        // Verify the conversion - note: from_offset adds 1 to avoid zero values
        assert_eq!(otel_stack.location_indices, vec![41, 51, 61]);
    }
}
