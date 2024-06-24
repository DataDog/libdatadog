// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tests {
    use serde_json;
    use crate::pb::{is_default, Span};

    #[test]
    fn test_is_default() {
        assert_eq!(true, is_default(&false));
        assert_eq!(false, is_default(&true));

        assert_eq!(true, is_default(&0));
        assert_eq!(false, is_default(&1));

        assert_eq!(true, is_default(&""));
        assert_eq!(false, is_default(&"foo"));
    }

    #[test]
    fn test_serialize_span() {
        let mut span = Span::default();
        span.name = "test".to_string();

        let json = serde_json::to_string(&span).unwrap();
        let expected = "{\"service\":\"\",\"name\":\"test\",\"resource\":\"\",\"trace_id\":0,\"span_id\":0,\"parent_id\":0,\"start\":0,\"duration\":0,\"meta\":{},\"metrics\":{},\"type\":\"\"}";
        assert_eq!(expected, json);

        span.error = 42;
        let json = serde_json::to_string(&span).unwrap();
        let expected = "{\"service\":\"\",\"name\":\"test\",\"resource\":\"\",\"trace_id\":0,\"span_id\":0,\"parent_id\":0,\"start\":0,\"duration\":0,\"error\":42,\"meta\":{},\"metrics\":{},\"type\":\"\"}";
        assert_eq!(expected, json);
    }
}
