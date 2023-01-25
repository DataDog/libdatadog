#[cfg(test)]
mod normalize_tests {

    use std::time::SystemTime;
    use std::collections::HashMap;
    use rand::Rng;
    use crate::pb;
    use crate::normalizer;
    use crate::normalize_utils;

    pub fn new_test_span() -> pb::Span {
        let mut rng = rand::thread_rng();

        return pb::Span {
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
    pub fn test_normalize_ok() {
        let mut test_span = new_test_span();
        assert!(normalizer::normalize(&mut test_span).is_ok());
    }

    #[test]
    pub fn test_normalize_service_passes() {
        let mut test_span = new_test_span();
        let before_service = test_span.service.clone();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_service, test_span.service);
    }

    #[test]
    pub fn test_normalize_empty_service_no_lang() {
        let mut test_span = new_test_span();
        test_span.service = "".to_string();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.service, normalize_utils::DEFAULT_SERVICE_NAME);
    }

    // TODO: Add a test for normalizing a span with an empty service, but has a language specified.
    //       Need to implement passing the tag stats as the second parameter of the normalize function,
    //       and pass the language specified in tag stats into normalize_service.

    #[test]
    pub fn test_normalize_long_service() {
        let mut test_span = new_test_span();
        test_span.service = "CAMEMBERT".repeat(100).to_string();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert!(test_span.service.len() == normalize_utils::MAX_SERVICE_LEN as usize);
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
        assert_eq!(test_span.name, normalize_utils::DEFAULT_SPAN_NAME);
    }

    #[test]
    pub fn test_normalize_long_name() {
        let mut test_span = new_test_span();
        test_span.name = "CAMEMBERT".repeat(100).to_string();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert!(test_span.name.len() == normalize_utils::MAX_NAME_LEN as usize);
    }

    #[test]
    pub fn test_normalize_name_no_alphanumeric() {
        let mut test_span = new_test_span();
        test_span.name = "/".to_string();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.name, normalize_utils::DEFAULT_SPAN_NAME);
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

    #[test]
    pub fn test_normalize_trace_id_passes() {
        let mut test_span = new_test_span();
        let before_trace_id = test_span.trace_id.clone();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_trace_id, test_span.trace_id);
    }

    #[test]
    pub fn test_normalize_no_trace_id() {
        let mut test_span = new_test_span();
        test_span.trace_id = 0;
        assert!(normalizer::normalize(&mut test_span).is_err());
    }

    #[test]
    pub fn test_normalize_component_to_name() {
        let mut test_span = new_test_span();
        let before_trace_id = test_span.trace_id.clone();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_trace_id, test_span.trace_id);
    }

    // TODO: Add a unit test for testing Component2Name, one that is 
    //       implemented within the normalize function.

    #[test]
    pub fn test_normalize_span_id_passes() {
        let mut test_span = new_test_span();
        let before_span_id = test_span.span_id.clone();
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_span_id, test_span.span_id);
    }

    #[test]
    pub fn test_normalize_no_span_id() {
        let mut test_span = new_test_span();
        test_span.span_id = 0;
        assert!(normalizer::normalize(&mut test_span).is_err());
    }

    #[test]
    pub fn test_normalize_start_passes() {
        let mut test_span = new_test_span();
        let before_start = test_span.start;
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_start, test_span.start);
    }

    fn get_current_time() -> i64 {
        return SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos() as i64;
    }

    #[test]
    pub fn test_normalize_start_too_small() {
        let mut test_span = new_test_span();

        test_span.start = 42;
        let min_start = get_current_time() - test_span.duration;

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert!(test_span.start >= min_start);
        assert!(test_span.start <= get_current_time());
    }

    #[test]
    pub fn test_normalize_start_too_small_with_large_duration() {
        let mut test_span = new_test_span();
        
        test_span.start = 42;
        test_span.duration = get_current_time() * 2;
        let min_start = get_current_time();
        
        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert!(test_span.start >= min_start); // start should have been reset to current time
        assert!(test_span.start <= get_current_time()); //start should have been reset to current time
    }

    #[test]
    pub fn test_normalize_duration_passes() {
        let mut test_span = new_test_span();
        let before_duration = test_span.duration.clone();

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_duration, test_span.duration);
    }

    #[test]
    pub fn test_normalize_empty_duration() {
        let mut test_span = new_test_span();
        test_span.duration = 0;

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.duration, 0);
    }

    #[test]
    pub fn test_normalize_negative_duration() {
        let mut test_span = new_test_span();
        test_span.duration = -50;

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.duration, 0);
    }

    #[test]
    pub fn test_normalize_large_duration() {
        let mut test_span = new_test_span();
        test_span.duration = std::i64::MAX;

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.duration, 0);
    }

    #[test]
    pub fn test_normalize_error_passes() {
        let mut test_span = new_test_span();
        let before_error = test_span.error.clone();

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_error, test_span.error);
    }

    #[test]
    pub fn test_normalize_metrics_passes() {
        let mut test_span = new_test_span();
        let before_metrics = test_span.metrics.clone();

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_metrics, test_span.metrics);
    }

    #[test]
    pub fn test_normalize_meta_passes() {
        let mut test_span = new_test_span();
        let before_meta = test_span.meta.clone();

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_meta, test_span.meta);
    }

    #[test]
    pub fn test_normalize_parent_id_passes() {
        let mut test_span = new_test_span();
        let before_parent_id = test_span.parent_id.clone();

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_parent_id, test_span.parent_id);
    }

    #[test]
    pub fn test_normalize_type_passes() {
        let mut test_span = new_test_span();
        let before_type = test_span.r#type.clone();

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(before_type, test_span.r#type);
    }

    #[test]
    pub fn test_normalize_type_too_long() {
        let mut test_span = new_test_span();
        test_span.r#type = "sql".repeat(1000);

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.r#type.len(), normalizer::MAX_TYPE_LEN as usize);
    }

    #[test]
    pub fn test_normalize_service_tag() {
        let mut test_span = new_test_span();
        test_span.service = "retargeting(api-Staging ".to_string();

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.service, "retargeting_api-staging");
    }

    #[test]
    pub fn test_normalize_env() {
        let mut test_span = new_test_span();
        test_span.meta.insert("env".to_string(), "DEVELOPMENT".to_string());

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!("development", test_span.meta.get("env").unwrap());
    }

    #[test]
    pub fn test_special_zipkin_root_span() {
        let mut test_span = new_test_span();
        test_span.parent_id = 42;
        test_span.trace_id = 42;
        test_span.span_id = 42;

        let before_trace_id = test_span.trace_id;
        let before_span_id = test_span.span_id;

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!(test_span.parent_id, 0);
        assert_eq!(test_span.trace_id, before_trace_id);
        assert_eq!(test_span.span_id, before_span_id);

    }

    #[test]
    pub fn test_normalize_trace_empty() {
        let mut test_span = new_test_span();
        test_span.meta.insert("env".to_string(), "DEVELOPMENT".to_string());

        assert!(normalizer::normalize(&mut test_span).is_ok());
        assert_eq!("development", test_span.meta.get("env").unwrap());
    }

    #[test]
    pub fn test_is_valid_status_code() {
        assert!(normalizer::is_valid_status_code("100".to_string()));
        assert!(normalizer::is_valid_status_code("599".to_string()));
        assert!(!normalizer::is_valid_status_code("99".to_string()));
        assert!(!normalizer::is_valid_status_code("600".to_string()));
        assert!(!normalizer::is_valid_status_code("Invalid status code".to_string()));
    }
}