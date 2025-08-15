// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::Id;
use crate::internal::Location as InternalLocation;

// For owned values - forward to reference version
impl From<InternalLocation> for datadog_profiling_otel::Location {
    fn from(internal_location: InternalLocation) -> Self {
        Self::from(&internal_location)
    }
}

// For references (existing implementation)
impl From<&InternalLocation> for datadog_profiling_otel::Location {
    fn from(internal_location: &InternalLocation) -> Self {
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
        // Create an internal location
        let internal_location = InternalLocation {
            mapping_id: Some(MappingId::from_offset(1)),
            function_id: FunctionId::from_offset(2),
            address: 0x1000,
            line: 42,
        };

        // Convert to OpenTelemetry Location
        let otel_location = datadog_profiling_otel::Location::from(&internal_location);

        // Verify the conversion - note: from_offset adds 1 to avoid zero values
        assert_eq!(otel_location.mapping_index, 2);
        assert_eq!(otel_location.address, 0x1000);
        assert_eq!(otel_location.line.len(), 1);
        assert_eq!(otel_location.line[0].function_index, 3);
        assert_eq!(otel_location.line[0].line, 42);
        assert_eq!(otel_location.line[0].column, 0);
        assert!(!otel_location.is_folded);
        assert_eq!(otel_location.attribute_indices, vec![] as Vec<i32>);
    }

    #[test]
    fn test_from_internal_location_no_mapping() {
        // Create an internal location without mapping
        let internal_location = InternalLocation {
            mapping_id: None,
            function_id: FunctionId::from_offset(5),
            address: 0x2000,
            line: 100,
        };

        // Convert to OpenTelemetry Location
        let otel_location = datadog_profiling_otel::Location::from(&internal_location);

        // Verify the conversion
        assert_eq!(otel_location.mapping_index, 0); // 0 represents no mapping
        assert_eq!(otel_location.address, 0x2000);
        assert_eq!(otel_location.line.len(), 1);
        assert_eq!(otel_location.line[0].function_index, 6);
        assert_eq!(otel_location.line[0].line, 100);
    }

    #[test]
    fn test_into_otel_location() {
        // Create an internal location
        let internal_location = InternalLocation {
            mapping_id: Some(MappingId::from_offset(10)),
            function_id: FunctionId::from_offset(20),
            address: 0x3000,
            line: 200,
        };

        // Convert using .into() method
        let otel_location: datadog_profiling_otel::Location = (&internal_location).into();

        // Verify the conversion - note: from_offset adds 1 to avoid zero values
        assert_eq!(otel_location.mapping_index, 11);
        assert_eq!(otel_location.address, 0x3000);
        assert_eq!(otel_location.line[0].function_index, 21);
        assert_eq!(otel_location.line[0].line, 200);
    }

    #[test]
    fn test_into_otel_location_owned() {
        // Create an internal location
        let internal_location = InternalLocation {
            mapping_id: Some(MappingId::from_offset(30)),
            function_id: FunctionId::from_offset(40),
            address: 0x4000,
            line: 300,
        };

        // Convert using .into() method with owned value
        let otel_location: datadog_profiling_otel::Location = internal_location.into();

        // Verify the conversion - note: from_offset adds 1 to avoid zero values
        assert_eq!(otel_location.mapping_index, 31);
        assert_eq!(otel_location.address, 0x4000);
        assert_eq!(otel_location.line[0].function_index, 41);
        assert_eq!(otel_location.line[0].line, 300);
    }
}
