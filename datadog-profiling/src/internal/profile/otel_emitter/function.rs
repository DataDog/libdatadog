// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::Id;
use crate::internal::Function as InternalFunction;

// For owned values - forward to reference version
impl From<InternalFunction> for datadog_profiling_otel::Function {
    fn from(internal_function: InternalFunction) -> Self {
        Self::from(&internal_function)
    }
}

// For references (existing implementation)
impl From<&InternalFunction> for datadog_profiling_otel::Function {
    fn from(internal_function: &InternalFunction) -> Self {
        Self {
            name_strindex: internal_function.name.to_raw_id() as i32,
            system_name_strindex: internal_function.system_name.to_raw_id() as i32,
            filename_strindex: internal_function.filename.to_raw_id() as i32,
            start_line: 0, // Not available in internal Function
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::identifiable::StringId;

    #[test]
    fn test_from_internal_function() {
        // Create an internal function
        let internal_function = InternalFunction {
            name: StringId::from_offset(0),
            system_name: StringId::from_offset(1),
            filename: StringId::from_offset(2),
        };

        // Convert to OpenTelemetry Function
        let otel_function = datadog_profiling_otel::Function::from(&internal_function);

        // Verify the conversion - note: StringId doesn't add 1, it's direct conversion
        assert_eq!(otel_function.name_strindex, 0);
        assert_eq!(otel_function.system_name_strindex, 1);
        assert_eq!(otel_function.filename_strindex, 2);
        assert_eq!(otel_function.start_line, 0);
    }

    #[test]
    fn test_from_internal_function_with_large_offsets() {
        // Create an internal function with large offsets
        let internal_function = InternalFunction {
            name: StringId::from_offset(999999),
            system_name: StringId::from_offset(888888),
            filename: StringId::from_offset(777777),
        };

        // Convert to OpenTelemetry Function
        let otel_function = datadog_profiling_otel::Function::from(&internal_function);

        // Verify the conversion
        assert_eq!(otel_function.name_strindex, 999999);
        assert_eq!(otel_function.system_name_strindex, 888888);
        assert_eq!(otel_function.filename_strindex, 777777);
        assert_eq!(otel_function.start_line, 0);
    }

    #[test]
    fn test_into_otel_function() {
        // Create an internal function
        let internal_function = InternalFunction {
            name: StringId::from_offset(100),
            system_name: StringId::from_offset(200),
            filename: StringId::from_offset(300),
        };

        // Convert using .into() method
        let otel_function: datadog_profiling_otel::Function = (&internal_function).into();

        // Verify the conversion
        assert_eq!(otel_function.name_strindex, 100);
        assert_eq!(otel_function.system_name_strindex, 200);
        assert_eq!(otel_function.filename_strindex, 300);
        assert_eq!(otel_function.start_line, 0);
    }

    #[test]
    fn test_into_otel_function_owned() {
        // Create an internal function
        let internal_function = InternalFunction {
            name: StringId::from_offset(400),
            system_name: StringId::from_offset(500),
            filename: StringId::from_offset(600),
        };

        // Convert using .into() method with owned value
        let otel_function: datadog_profiling_otel::Function = internal_function.into();

        // Verify the conversion
        assert_eq!(otel_function.name_strindex, 400);
        assert_eq!(otel_function.system_name_strindex, 500);
        assert_eq!(otel_function.filename_strindex, 600);
        assert_eq!(otel_function.start_line, 0);
    }
}
