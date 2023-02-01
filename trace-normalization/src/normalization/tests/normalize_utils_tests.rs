// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

#[cfg(test)]
mod normalize_tests {

    use crate::normalize_utils;

    #[test]
    fn test_normalize_name() {
        let test_tuples: [(&str, &str, &str); 5] = [
            (
                "",
                "",
                "Normalizer Error: Empty"
            ),
            (
                "good",
                "good",
                "",
            ),
            (
                "Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.Too-Long-.",
                "Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.Too_Long.",
                "",
            ),
            (
                "bad-name",
                "bad_name",
                "",
            ),
            (
                "&",
                "",
                "Normalizer Error: Invalid"
            )
        ];

        for tuple in test_tuples.iter() {
            let input = tuple.0;
            let expected = tuple.1;
            let expected_err = tuple.2;

            match normalize_utils::normalize_name(input.to_string()) {
                Ok(val) => {
                    assert_eq!(expected_err, "");
                    assert_eq!(val, expected);
                },
                Err(err) => {
                    assert_eq!(format!("{}", err), expected_err);
                }
            }
        }
    }
}