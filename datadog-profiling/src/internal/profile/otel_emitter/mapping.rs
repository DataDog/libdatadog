// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::Id;
use crate::internal::Mapping as InternalMapping;

// For owned values - forward to reference version
impl From<InternalMapping> for datadog_profiling_otel::Mapping {
    fn from(internal_mapping: InternalMapping) -> Self {
        Self::from(&internal_mapping)
    }
}

// For references (existing implementation)
impl From<&InternalMapping> for datadog_profiling_otel::Mapping {
    fn from(internal_mapping: &InternalMapping) -> Self {
        Self {
            memory_start: internal_mapping.memory_start,
            memory_limit: internal_mapping.memory_limit,
            file_offset: internal_mapping.file_offset,
            filename_strindex: internal_mapping.filename.to_raw_id() as i32,
            attribute_indices: vec![], // Not available in internal Mapping
            has_functions: true,       // Assume true since we have function information
            has_filenames: true,       // Assume true since we have filename
            has_line_numbers: true,    // Assume true since we have line information
            has_inline_frames: false,  // Not available in internal Mapping
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::identifiable::StringId;

    #[test]
    fn test_from_internal_mapping() {
        // Create an internal mapping
        let internal_mapping = InternalMapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0x100,
            filename: StringId::from_offset(42),
            build_id: StringId::from_offset(123),
        };

        // Convert to OpenTelemetry Mapping
        let otel_mapping = datadog_profiling_otel::Mapping::from(&internal_mapping);

        // Verify the conversion
        assert_eq!(otel_mapping.memory_start, 0x1000);
        assert_eq!(otel_mapping.memory_limit, 0x2000);
        assert_eq!(otel_mapping.file_offset, 0x100);
        assert_eq!(otel_mapping.filename_strindex, 42);
        assert_eq!(otel_mapping.attribute_indices, vec![] as Vec<i32>);
        assert!(otel_mapping.has_functions);
        assert!(otel_mapping.has_filenames);
        assert!(otel_mapping.has_line_numbers);
        assert!(!otel_mapping.has_inline_frames);
    }

    #[test]
    fn test_from_internal_mapping_large_values() {
        // Create an internal mapping with large values
        let internal_mapping = InternalMapping {
            memory_start: 0x1000000000000000,
            memory_limit: 0x2000000000000000,
            file_offset: 0x1000000000000000,
            filename: StringId::from_offset(999),
            build_id: StringId::from_offset(888),
        };

        // Convert to OpenTelemetry Mapping
        let otel_mapping = datadog_profiling_otel::Mapping::from(&internal_mapping);

        // Verify the conversion
        assert_eq!(otel_mapping.memory_start, 0x1000000000000000);
        assert_eq!(otel_mapping.memory_limit, 0x2000000000000000);
        assert_eq!(otel_mapping.file_offset, 0x1000000000000000);
        assert_eq!(otel_mapping.filename_strindex, 999);
    }

    #[test]
    fn test_into_otel_mapping() {
        // Create an internal mapping
        let internal_mapping = InternalMapping {
            memory_start: 0x3000,
            memory_limit: 0x4000,
            file_offset: 0x200,
            filename: StringId::from_offset(555),
            build_id: StringId::from_offset(666),
        };

        // Convert using .into() method
        let otel_mapping: datadog_profiling_otel::Mapping = (&internal_mapping).into();

        // Verify the conversion
        assert_eq!(otel_mapping.memory_start, 0x3000);
        assert_eq!(otel_mapping.memory_limit, 0x4000);
        assert_eq!(otel_mapping.file_offset, 0x200);
        assert_eq!(otel_mapping.filename_strindex, 555);
    }

    #[test]
    fn test_into_otel_mapping_owned() {
        // Create an internal mapping
        let internal_mapping = InternalMapping {
            memory_start: 0x5000,
            memory_limit: 0x6000,
            file_offset: 0x300,
            filename: StringId::from_offset(777),
            build_id: StringId::from_offset(888),
        };

        // Convert using .into() method with owned value
        let otel_mapping: datadog_profiling_otel::Mapping = internal_mapping.into();

        // Verify the conversion
        assert_eq!(otel_mapping.memory_start, 0x5000);
        assert_eq!(otel_mapping.memory_limit, 0x6000);
        assert_eq!(otel_mapping.file_offset, 0x300);
        assert_eq!(otel_mapping.filename_strindex, 777);
    }
}
