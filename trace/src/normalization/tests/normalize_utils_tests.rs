// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

#[cfg(test)]
mod normalize_tests {

    use crate::errors;
    use crate::normalize_utils;

    #[test]
    fn test_normalize_name() {
        let test_tuples: [(&str, &str, Option<errors::NormalizeErrors>); 4] = [
            (
                "",
                normalize_utils::DEFAULT_SPAN_NAME,
                Some(errors::NormalizeErrors::ErrorEmpty),
            ),
            (
                "good",
                "good",
                None,
            ),
            (
                "Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.",
                "Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.",
                Some(errors::NormalizeErrors::ErrorTooLong),
            ),
            (
                "bad-name",
                "bad_name",
                None,
            ),
        ];

        for tuple in test_tuples.iter() {
            let input = tuple.0;
            let expected = tuple.1;
            let expected_err = tuple.2.clone();
            let result = normalize_utils::normalize_name(input.to_string());

            assert_eq!(result.0, expected.to_string());

            match result.1 {
                Some(res) => {
                    assert!(expected_err.is_some());
                    assert_eq!(res, expected_err.unwrap())
                },
                None => assert!(expected_err.is_none())
            }
        }
    }
}