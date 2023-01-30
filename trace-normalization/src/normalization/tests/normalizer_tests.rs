// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

#[cfg(test)]
mod normalize_tests {

    use std::collections::HashMap;
    use rand::Rng;
    use crate::pb;
    use crate::normalizer;
    use crate::normalize_utils;

    pub fn new_test_span() -> pb::Span {
        let mut rng = rand::thread_rng();

        pb::Span {
            duration: 10000000,
            error: 0,
            resource: "GET /some/raclette".to_string(),
            service: "django".to_string(),
            name: "django.controller".to_string(),
            span_id: rng.gen(),
            start: 1448466874000000000,
            trace_id: 424242,
            meta: HashMap::from([
                ("user".to_string(), "leo".to_string()),
                ("pool".to_string(), "fondue".to_string())
            ]),
            metrics: HashMap::from([
                ("cheese_weight".to_string(), 100000.0)
            ]),
            parent_id: 1111,
            r#type: "http".to_string(),
            meta_struct: HashMap::new()
        }
    }

    #[test]
    pub fn test_normalize_name_passes() {
        let mut test_span = new_test_span();
        let before_name = test_span.name.clone();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_name, test_span.name);
    }

    #[test]
    pub fn test_normalize_empty_name() {
        let mut test_span = new_test_span();
        test_span.name = "".to_string();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.name, normalizer::DEFAULT_SPAN_NAME);
    }

    #[test]
    pub fn test_normalize_long_name() {
        let mut test_span = new_test_span();
        test_span.name = "CAMEMBERT".repeat(100);
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert!(test_span.name.len() == normalize_utils::MAX_NAME_LEN);
    }

    #[test]
    pub fn test_normalize_name_no_alphanumeric() {
        let mut test_span = new_test_span();
        test_span.name = "/".to_string();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.name, normalizer::DEFAULT_SPAN_NAME);
    }

    #[test]
    pub fn test_normalize_name_for_metrics() {
        let expected_names = HashMap::from([
            ("pylons.controller".to_string(), "pylons.controller".to_string()),
            ("trace-api.request".to_string(), "trace_api.request".to_string())
        ]);

        let mut test_span = new_test_span();
        for (name, expected_name) in expected_names {
            test_span.name = name;
            assert!(normalizer::normalize(&mut test_span).is_ok());
            assert_eq!(test_span.name, expected_name);
        }
    }

    #[test]
    pub fn test_normalize_resource_passes() {
        let mut test_span = new_test_span();
        let before_resource = test_span.resource.clone();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_resource, test_span.resource);
    }

    #[test]
    pub fn test_normalize_empty_resource() {
        let mut test_span = new_test_span();
        test_span.resource = "".to_string();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.resource, test_span.name);
    }
}