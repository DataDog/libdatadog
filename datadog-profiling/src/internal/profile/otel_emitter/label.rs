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
            let unit_index = num_unit.to_raw_id() as usize;

            // Only add to the map if the unit is not the default empty string (index 0)
            if unit_index > 0 {
                key_to_unit_map.insert(key_id, unit_index);
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
    fn test_convert_label() {
        // Test string label conversion
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
        let key_value =
            convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map).unwrap();
        assert_eq!(key_value.key, "thread_id");
        let s = match key_value.value.expect("Expected Some value") {
            datadog_profiling_otel::key_value::Value::StringValue(s) => s,
            _ => panic!("Expected StringValue"),
        };
        assert_eq!(s, "main");

        // Test numeric label with unit mapping
        let label = InternalLabel::num(
            StringId::from_offset(1), // "thread_id" (reusing key)
            1024,                     // 1024
            StringId::from_offset(2), // "main" (reusing as unit)
        );

        let key_value =
            convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map).unwrap();
        assert_eq!(key_value.key, "thread_id");
        let n = match key_value.value.expect("Expected Some value") {
            datadog_profiling_otel::key_value::Value::IntValue(n) => n,
            _ => panic!("Expected IntValue"),
        };
        assert_eq!(n, 1024);

        // Verify unit mapping was added
        assert_eq!(key_to_unit_map.get(&1), Some(&2));

        // Test numeric label without unit mapping (empty string unit)
        let label = InternalLabel::num(
            StringId::from_offset(1), // "thread_id"
            42,                       // 42
            StringId::from_offset(0), // empty string (default)
        );

        let key_value =
            convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map).unwrap();
        assert_eq!(key_value.key, "thread_id");
        let n = match key_value.value.expect("Expected Some value") {
            datadog_profiling_otel::key_value::Value::IntValue(n) => n,
            _ => panic!("Expected IntValue"),
        };
        assert_eq!(n, 42);

        // Unit mapping should still exist from previous test
        assert_eq!(key_to_unit_map.get(&1), Some(&2));
    }

    #[test]
    fn test_convert_label_errors() {
        // Test out of bounds key
        let string_table = vec!["".to_string()]; // Only one string

        let label = InternalLabel::str(
            StringId::from_offset(1), // This index doesn't exist
            StringId::from_offset(0), // This index exists
        );

        let mut key_to_unit_map = HashMap::new();
        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_err());

        // Test empty string table
        let string_table: Vec<String> = vec![];

        let label = InternalLabel::str(
            StringId::from_offset(0), // Even index 0 is out of bounds
            StringId::from_offset(0),
        );

        let result = convert_label_to_key_value(&label, &string_table, &mut key_to_unit_map);
        assert!(result.is_err());
    }
}
