#[cfg(test)]
mod normalize_tests {

    use crate::errors;
    use crate::normalize_utils;

    #[test]
    fn test_normalize_tag() {
        let test_tuples = [
            ("#test_starting_hash", "test_starting_hash"),
            ("TestCAPSandSuch", "testcapsandsuch"),
            ("Test Conversion Of Weird !@#$%^&**() Characters", "test_conversion_of_weird_characters"),
            ("$#weird_starting", "weird_starting"),
            ("allowed:c0l0ns", "allowed:c0l0ns"),
            ("1love", "love"),
            ("ünicöde", "ünicöde"),
            ("ünicöde:metäl", "ünicöde:metäl"),
            ("Data🐨dog🐶 繋がっ⛰てて", "data_dog_繋がっ_てて"),
            (" spaces   ", "spaces"),
            (" #hashtag!@#spaces #__<>#  ", "hashtag_spaces"),
            (":testing", ":testing"),
            ("_foo", "foo"),
            (":::test", ":::test"),
            ("contiguous_____underscores", "contiguous_underscores"),
            ("foo_", "foo"),
            ("\u{017F}odd_\u{017F}case\u{017F}", "\u{017F}odd_\u{017F}case\u{017F}"), // edge-case
            ("", ""),
            (" ", ""),
            ("ok", "ok"),
            ("™Ö™Ö™™Ö™", "ö_ö_ö"),
            ("AlsO:ök", "also:ök"),
            (":still_ok", ":still_ok"),
            ("___trim", "trim"),
            ("12.:trim@", ":trim"),
            ("12.:trim@@", ":trim"),
            ("fun:ky__tag/1", "fun:ky_tag/1"),
            ("fun:ky@tag/2", "fun:ky_tag/2"),
            ("fun:ky@@@tag/3", "fun:ky_tag/3"),
            ("tag:1/2.3", "tag:1/2.3"),
            ("---fun:k####y_ta@#g/1_@@#", "fun:k_y_ta_g/1"),
            ("AlsO:œ#@ö))œk", "also:œ_ö_œk"),
            // these two tests from normalize_test.go are ommitted as rust requires characters to be in range [\x00-\x7f]
            // ("test\x99\x8faaa", "test_aaa"),
            // ("test\x99\x8f", "test"),
            (&("a".repeat(888)), &("a".repeat(200))),
            (&("a".to_string() + &("🐶".repeat(799))), "a"),
            (&("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string()), "a"),
            (&("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string() + &char::REPLACEMENT_CHARACTER.to_string()), "a"),
            (&("a".to_string() + &char::REPLACEMENT_CHARACTER.to_string() + &char::REPLACEMENT_CHARACTER.to_string() + "b"), "a_b"),
            (
                "A00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 000000000000",
                "a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000_0"
            )
        ];

        for tuple in test_tuples.iter() {
            let input = tuple.0;
            let expected = tuple.1;
            assert_eq!(normalize_utils::normalize_tag(input.to_string()), expected.to_string());
        }
    }

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

    #[test]
    fn test_normalize_service() {
        let test_tuples: [(&str, &str, Option<errors::NormalizeErrors>); 4] = [
            (
                "",
                normalize_utils::DEFAULT_SERVICE_NAME,
                Some(errors::NormalizeErrors::ErrorEmpty),
            ),
            (
                "good",
                "good",
                None,
            ),
            (
                "Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.Too$Long$.",
                "too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.too_long_.",
                Some(errors::NormalizeErrors::ErrorTooLong),
            ),
            (
                "bad$service",
                "bad_service",
                None,
            ),
        ];

        for tuple in test_tuples.iter() {
            let input = tuple.0;
            let expected = tuple.1;
            let expected_err = tuple.2.clone();
            let result = normalize_utils::normalize_service(input.to_string(), "".to_string());

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