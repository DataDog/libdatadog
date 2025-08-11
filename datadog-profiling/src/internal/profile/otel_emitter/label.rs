// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! All the mess here will go away once https://github.com/open-telemetry/opentelemetry-proto/pull/672/files is merged.

use crate::collections::identifiable::Id;
use crate::internal::Label as InternalLabel;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// Converts a datadog-profiling internal Label to an OpenTelemetry KeyValue
///
/// # Arguments
/// * `label` - The internal label to convert
/// * `string_table` - A slice of strings where StringIds index into
/// * `key_to_unit_map` - A mutable map from key string index to unit string index for numeric
///   labels
///
/// # Returns
/// * `Ok(KeyValue)` if the conversion is successful
/// * `Err` with context if the StringIds are out of bounds of the string table
pub fn convert_label_to_key_value(
    label: &InternalLabel,
    string_table: &[String],
    key_to_unit_map: &mut HashMap<usize, usize>,
) -> Result<datadog_profiling_otel::KeyValue> {
    // Get the key string
    let key_id = label.get_key().to_raw_id() as usize;
    let key = string_table
        .get(key_id)
        .with_context(|| {
            format!(
                "Key string index {} out of bounds (string table has {} elements)",
                key_id,
                string_table.len()
            )
        })?
        .to_string();

    match label.get_value() {
        crate::internal::LabelValue::Str(str_id) => {
            let str_value_id = str_id.to_raw_id() as usize;
            let str_value = string_table
                .get(str_value_id)
                .with_context(|| {
                    format!(
                        "Value string index {} out of bounds (string table has {} elements)",
                        str_value_id,
                        string_table.len()
                    )
                })?
                .to_string();

            Ok(datadog_profiling_otel::KeyValue {
                key,
                value: Some(datadog_profiling_otel::key_value::Value::StringValue(
                    str_value,
                )),
            })
        }
        crate::internal::LabelValue::Num { num, num_unit } => {
            // Note: OpenTelemetry KeyValue doesn't support units, so we only store the numeric
            // value But we track the mapping for building the attribute_units table
            let key_index = label.get_key().to_raw_id() as usize;
            let unit_index = num_unit.to_raw_id() as usize;

            // Only add to the map if the unit is not the default empty string (index 0)
            if unit_index > 0 {
                key_to_unit_map.insert(key_index, unit_index);
            }

            Ok(datadog_profiling_otel::KeyValue {
                key,
                value: Some(datadog_profiling_otel::key_value::Value::IntValue(*num)),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::identifiable::StringId;

    #[test]
    fn test_convert_string_label() {
        let string_table = vec![
            "".to_string(),          // index 0
            "thread_id".to_string(), // index 1
            "main".to_string(),      // index 2
        ];

        let label = InternalLabel::str(
            StringId::from_offset(1), // "thread_id"
            StringId::from_offset(2), // "main"
        );

        let mut key_to_unit_map = HashMap::new();
        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_ok());

        let key_value = result.unwrap();
        assert_eq!(key_value.key, "thread_id");
        match key_value.value {
            Some(datadog_profiling_otel::key_value::Value::StringValue(s)) => {
                assert_eq!(s, "main");
            }
            _ => panic!("Expected StringValue"),
        }
    }

    #[test]
    fn test_convert_numeric_label() {
        let string_table = vec![
            "".to_string(),                // index 0
            "allocation_size".to_string(), // index 1
            "bytes".to_string(),           // index 2
        ];

        let label = InternalLabel::num(
            StringId::from_offset(1), // "allocation_size"
            1024,                     // 1024 bytes
            StringId::from_offset(2), // "bytes"
        );

        let mut key_to_unit_map = HashMap::new();
        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_ok());

        let key_value = result.unwrap();
        assert_eq!(key_value.key, "allocation_size");
        match key_value.value {
            Some(datadog_profiling_otel::key_value::Value::IntValue(n)) => {
                assert_eq!(n, 1024);
            }
            _ => panic!("Expected IntValue"),
        }
    }

    #[test]
    fn test_convert_label_out_of_bounds() {
        let string_table = vec!["".to_string()]; // Only one string

        let label = InternalLabel::str(
            StringId::from_offset(1), // This index doesn't exist
            StringId::from_offset(0), // This index exists
        );

        let mut key_to_unit_map = HashMap::new();
        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_err());
    }

    #[test]
    fn test_convert_label_empty_string_table() {
        let string_table: Vec<String> = vec![];

        let label = InternalLabel::str(
            StringId::from_offset(0), // Even index 0 is out of bounds
            StringId::from_offset(0),
        );

        let mut key_to_unit_map = HashMap::new();
        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_err());
    }

    #[test]
    fn test_convert_numeric_label_with_unit_mapping() {
        let string_table = vec![
            "".to_string(),             // index 0
            "memory_usage".to_string(), // index 1
            "megabytes".to_string(),    // index 2
        ];

        let label = InternalLabel::num(
            StringId::from_offset(1), // "memory_usage"
            512,                      // 512 MB
            StringId::from_offset(2), // "megabytes"
        );

        let mut key_to_unit_map = HashMap::new();
        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_ok());

        // Verify the KeyValue conversion
        let key_value = result.unwrap();
        assert_eq!(key_value.key, "memory_usage");
        match key_value.value {
            Some(datadog_profiling_otel::key_value::Value::IntValue(n)) => {
                assert_eq!(n, 512);
            }
            _ => panic!("Expected IntValue"),
        }

        // Verify the unit mapping was added
        assert_eq!(key_to_unit_map.get(&1), Some(&2)); // key index 1 maps to unit index 2
    }

    #[test]
    fn test_convert_numeric_label_without_unit_mapping() {
        let string_table = vec![
            "".to_string(),        // index 0
            "counter".to_string(), // index 1
        ];

        let label = InternalLabel::num(
            StringId::from_offset(1), // "counter"
            42,                       // 42
            StringId::from_offset(0), // empty string (default)
        );

        let mut key_to_unit_map = HashMap::new();
        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_ok());

        // Verify the KeyValue conversion
        let key_value = result.unwrap();
        assert_eq!(key_value.key, "counter");
        match key_value.value {
            Some(datadog_profiling_otel::key_value::Value::IntValue(n)) => {
                assert_eq!(n, 42);
            }
            _ => panic!("Expected IntValue"),
        }

        // Verify no unit mapping was added for default empty string
        assert!(key_to_unit_map.is_empty());
    }
}
