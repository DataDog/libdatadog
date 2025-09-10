// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::Id;
use crate::internal;

// For owned values - forward to reference version
impl From<internal::Location> for datadog_profiling_otel::Location {
    fn from(internal_location: internal::Location) -> Self {
        Self::from(&internal_location)
    }
}

// For references (existing implementation)
impl From<&internal::Location> for datadog_profiling_otel::Location {
    fn from(internal_location: &internal::Location) -> Self {
        Self {
            mapping_index: internal_location
                .mapping_id
                .map(|id| id.to_raw_id() as i32)
                .unwrap_or(0), // 0 represents no mapping
            address: internal_location.address,
            line: vec![datadog_profiling_otel::Line {
                function_index: internal_location.function_id.to_raw_id() as i32,
                line: internal_location.line,
                column: 0, // Not available in internal Location
            }],
            is_folded: false,          // Not available in internal Location
            attribute_indices: vec![], // Not available in internal Location
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::{FunctionId, MappingId};

    #[test]
    fn test_from_internal_location() {
        // Test with mapping
        let internal_location = internal::Location {
            mapping_id: Some(MappingId::from_offset(1)),
            function_id: FunctionId::from_offset(2),
            address: 0x1000,
            line: 42,
        };

        let otel_location = datadog_profiling_otel::Location::from(&internal_location);
        assert_eq!(otel_location.mapping_index, 2);
        assert_eq!(otel_location.address, 0x1000);
        assert_eq!(otel_location.line.len(), 1);
        assert_eq!(otel_location.line[0].function_index, 3);
        assert_eq!(otel_location.line[0].line, 42);
        assert_eq!(otel_location.line[0].column, 0);
        assert!(!otel_location.is_folded);
        assert_eq!(otel_location.attribute_indices, vec![] as Vec<i32>);

        // Test without mapping
        let internal_location = internal::Location {
            mapping_id: None,
            function_id: FunctionId::from_offset(5),
            address: 0x2000,
            line: 100,
        };

        let otel_location = datadog_profiling_otel::Location::from(&internal_location);
        assert_eq!(otel_location.mapping_index, 0); // 0 represents no mapping
        assert_eq!(otel_location.address, 0x2000);
        assert_eq!(otel_location.line.len(), 1);
        assert_eq!(otel_location.line[0].function_index, 6);
        assert_eq!(otel_location.line[0].line, 100);
    }
}
