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
        // Test basic conversion
        let internal_function = InternalFunction {
            name: StringId::from_offset(0),
            system_name: StringId::from_offset(1),
            filename: StringId::from_offset(2),
        };

        let otel_function = datadog_profiling_otel::Function::from(&internal_function);
        assert_eq!(otel_function.name_strindex, 0);
        assert_eq!(otel_function.system_name_strindex, 1);
        assert_eq!(otel_function.filename_strindex, 2);
        assert_eq!(otel_function.start_line, 0);

        // Test with large offsets
        let internal_function = InternalFunction {
            name: StringId::from_offset(999999),
            system_name: StringId::from_offset(888888),
            filename: StringId::from_offset(777777),
        };

        let otel_function = datadog_profiling_otel::Function::from(&internal_function);
        assert_eq!(otel_function.name_strindex, 999999);
        assert_eq!(otel_function.system_name_strindex, 888888);
        assert_eq!(otel_function.filename_strindex, 777777);
        assert_eq!(otel_function.start_line, 0);
    }
}
