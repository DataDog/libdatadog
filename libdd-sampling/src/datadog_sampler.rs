// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::dd_constants::{
    RL_EFFECTIVE_RATE, SAMPLING_AGENT_RATE_TAG_KEY, SAMPLING_DECISION_MAKER_TAG_KEY,
    SAMPLING_KNUTH_RATE_TAG_KEY, SAMPLING_PRIORITY_TAG_KEY, SAMPLING_RULE_RATE_TAG_KEY,
};
use crate::dd_sampling::{mechanism, priority, SamplingMechanism, SamplingPriority};
use crate::sampling_rule_config::SamplingRuleConfig;

/// Type alias for sampling rules update callback
/// Consolidated callback type used across crates for remote config sampling updates
pub type SamplingRulesCallback = Box<dyn for<'a> Fn(&'a [SamplingRuleConfig]) + Send + Sync>;

use crate::types::{SamplingData, SpanProperties};

use super::agent_service_sampler::{AgentRates, ServicesSampler};
use super::rate_limiter::RateLimiter;
use super::rules_sampler::RulesSampler;
use super::sampling_rule::SamplingRule;

/// A composite sampler that applies rules in order of precedence
#[derive(Clone, Debug)]
pub struct DatadogSampler {
    /// Sampling rules to apply, in order of precedence
    rules: RulesSampler,

    /// Service-based samplers provided by the Agent
    service_samplers: ServicesSampler,

    /// Rate limiter for limiting the number of spans per second
    rate_limiter: RateLimiter,
}

impl DatadogSampler {
    /// Creates a new DatadogSampler with the given rules
    pub fn new(rules: Vec<SamplingRule>, rate_limit: i32) -> Self {
        // Create rate limiter with default value of 100 if not provided
        let limiter = RateLimiter::new(rate_limit, None);

        DatadogSampler {
            rules: RulesSampler::new(rules),
            service_samplers: ServicesSampler::default(),
            rate_limiter: limiter,
        }
    }

    // used for tests
    #[allow(dead_code)]
    pub(crate) fn update_service_rates(&self, rates: impl IntoIterator<Item = (String, f64)>) {
        self.service_samplers.update_rates(rates);
    }

    pub fn on_agent_response(&self) -> Box<dyn for<'a> Fn(&'a str) + Send + Sync> {
        let service_samplers = self.service_samplers.clone();
        Box::new(move |s: &str| {
            let Ok(new_rates) = serde_json::de::from_str::<AgentRates>(s) else {
                return;
            };
            let Some(new_rates) = new_rates.rate_by_service else {
                return;
            };
            service_samplers.update_rates(new_rates.into_iter().map(|(k, v)| (k.to_string(), v)));
        })
    }

    /// Creates a callback for updating sampling rules from remote configuration
    /// # Returns
    /// A boxed function that takes a slice of SamplingRuleConfig and updates the sampling rules
    pub fn on_rules_update(&self) -> SamplingRulesCallback {
        let rules_sampler = self.rules.clone();
        Box::new(move |rule_configs: &[SamplingRuleConfig]| {
            let new_rules = SamplingRule::from_configs(rule_configs.to_vec());

            rules_sampler.update_rules(new_rules);
        })
    }

    /// Computes a key for service-based sampling
    fn service_key(&self, span: &impl SpanProperties) -> String {
        // Get service from span
        let service = span.service().into_owned();
        // Get env from span
        let env = span.env();

        format!("service:{service},env:{env}")
    }

    /// Finds the highest precedence rule that matches the span
    fn find_matching_rule(&self, span: &impl SpanProperties) -> Option<SamplingRule> {
        self.rules.find_matching_rule(|rule| rule.matches(span))
    }

    /// Returns the sampling mechanism used for the decision
    fn get_sampling_mechanism(
        &self,
        rule: Option<&SamplingRule>,
        used_agent_sampler: bool,
    ) -> SamplingMechanism {
        if let Some(rule) = rule {
            match rule.provenance.as_str() {
                // Provenance will not be set for rules until we implement remote configuration
                "customer" => mechanism::REMOTE_USER_TRACE_SAMPLING_RULE,
                "dynamic" => mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE,
                _ => mechanism::LOCAL_USER_TRACE_SAMPLING_RULE,
            }
        } else if used_agent_sampler {
            // If using service-based sampling from the agent
            mechanism::AGENT_RATE_BY_SERVICE
        } else {
            // Should not happen, but just in case
            mechanism::DEFAULT
        }
    }

    /// Sample an incoming span based on the parent context and attributes
    pub fn sample(&self, data: &impl SamplingData) -> DdSamplingResult {
        if let Some(is_parent_sampled) = data.is_parent_sampled() {
            let priority = match is_parent_sampled {
                false => priority::AUTO_REJECT,
                true => priority::AUTO_KEEP,
            };
            // If a parent exists, inherit its sampling decision and trace state
            return DdSamplingResult {
                priority,
                trace_root_info: None,
            };
        }

        // Apply rules-based sampling
        data.with_span_properties(self, |sampler, span| sampler.sample_root(data, span))
    }

    /// Sample the root span of a trace
    fn sample_root(
        &self,
        data: &impl SamplingData,
        span: &impl SpanProperties,
    ) -> DdSamplingResult {
        let mut is_keep = true;
        let mut used_agent_sampler = false;
        let sample_rate;
        let mut rl_effective_rate: Option<f64> = None;
        let trace_id = data.trace_id();

        // Find a matching rule
        let matching_rule = self.find_matching_rule(span);

        // Apply sampling logic
        if let Some(rule) = &matching_rule {
            // Get the sample rate from the rule
            sample_rate = rule.sample_rate;

            // First check if the span should be sampled according to the rule
            if !rule.sample(trace_id) {
                is_keep = false;
            // If the span should be sampled, then apply rate limiting
            } else if !self.rate_limiter.is_allowed() {
                is_keep = false;
                rl_effective_rate = Some(self.rate_limiter.effective_rate());
            }
        } else {
            // Try service-based sampling from Agent
            let service_key = self.service_key(span);
            if let Some(sampler) = self.service_samplers.get(&service_key) {
                // Use the service-based sampler
                used_agent_sampler = true;
                sample_rate = sampler.sample_rate(); // Get rate for reporting

                // Check if the service sampler decides to drop
                if !sampler.sample(trace_id) {
                    is_keep = false;
                }
            } else {
                // Default sample rate, should never happen in practice if agent provides rates
                sample_rate = 1.0;
                // Keep the default decision (RecordAndSample)
            }
        }

        // Determine the sampling mechanism
        let mechanism = self.get_sampling_mechanism(matching_rule.as_ref(), used_agent_sampler);

        DdSamplingResult {
            priority: mechanism.to_priority(is_keep),
            trace_root_info: Some(TraceRootSamplingInfo {
                mechanism,
                rate: sample_rate,
                rl_effective_rate,
            }),
        }
    }
}

/// Formats a sampling rate with up to 6 significant digits, stripping trailing zeros.
///
/// This matches the Go behavior of `strconv.FormatFloat(rate, 'g', 6, 64)`.
///
/// # Examples
/// - `1.0` → `Some("1")`
/// - `0.5` → `Some("0.5")`
/// - `0.7654321` → `Some("0.765432")`
/// - `0.100000` → `Some("0.1")`
/// - `-0.1` → `None`
/// - `1.1` → `None`
fn format_sampling_rate(rate: f64) -> Option<String> {
    if rate.is_nan() || !(0.0..=1.0).contains(&rate) {
        return None;
    }

    if rate == 0.0 {
        return Some("0".to_string());
    }

    let digits = 6_i32;
    let magnitude = rate.abs().log10().floor() as i32;
    let scale = 10f64.powi(digits - 1 - magnitude);
    let rounded = (rate * scale).round() / scale;

    // Determine decimal places needed for 6 significant digits
    let decimal_places = if magnitude >= digits - 1 {
        0
    } else {
        (digits - 1 - magnitude) as usize
    };

    let s = format!("{:.prec$}", rounded, prec = decimal_places);
    // Strip trailing zeros after decimal point
    Some(if s.contains('.') {
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        s.to_string()
    } else {
        s
    })
}

pub struct TraceRootSamplingInfo {
    mechanism: SamplingMechanism,
    rate: f64,
    rl_effective_rate: Option<f64>,
}

impl TraceRootSamplingInfo {
    /// Returns the sampling mechanism used for this trace root
    pub fn mechanism(&self) -> SamplingMechanism {
        self.mechanism
    }

    /// Returns the sample rate used for this trace root
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Returns the effective rate limit if rate limiting was applied
    pub fn rl_effective_rate(&self) -> Option<f64> {
        self.rl_effective_rate
    }
}

pub struct DdSamplingResult {
    priority: SamplingPriority,
    trace_root_info: Option<TraceRootSamplingInfo>,
}

impl DdSamplingResult {
    #[inline(always)]
    pub fn get_priority(&self) -> SamplingPriority {
        self.priority
    }

    pub fn get_trace_root_sampling_info(&self) -> &Option<TraceRootSamplingInfo> {
        &self.trace_root_info
    }

    /// Returns Datadog-specific sampling tags to be added as attributes
    ///
    /// # Parameters
    /// * `factory` - The attribute factory to use for creating attributes
    ///
    /// # Returns
    /// An optional vector of attributes to add to the sampling result
    pub fn to_dd_sampling_tags<F>(&self, factory: &F) -> Option<Vec<F::Attribute>>
    where
        F: crate::types::AttributeFactory,
    {
        let Some(root_info) = &self.trace_root_info else {
            return None; // No root info, return empty attributes
        };

        let mut result: Vec<F::Attribute>;
        // Add rate limiting tag if applicable
        if let Some(limit) = root_info.rl_effective_rate() {
            result = Vec::with_capacity(4);
            result.push(factory.create_f64(RL_EFFECTIVE_RATE, limit));
        } else {
            result = Vec::with_capacity(3);
        }

        // Add the sampling decision trace tag with the mechanism
        let mechanism = root_info.mechanism();
        result.push(factory.create_string(SAMPLING_DECISION_MAKER_TAG_KEY, mechanism.to_cow()));

        // Add the sample rate tag with the correct key based on the mechanism
        match mechanism {
            mechanism::AGENT_RATE_BY_SERVICE => {
                result.push(factory.create_f64(SAMPLING_AGENT_RATE_TAG_KEY, root_info.rate()));
                if let Some(rate_str) = format_sampling_rate(root_info.rate()) {
                    result.push(factory.create_string(
                        SAMPLING_KNUTH_RATE_TAG_KEY,
                        std::borrow::Cow::Owned(rate_str),
                    ));
                }
            }
            mechanism::REMOTE_USER_TRACE_SAMPLING_RULE
            | mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE
            | mechanism::LOCAL_USER_TRACE_SAMPLING_RULE => {
                result.push(factory.create_f64(SAMPLING_RULE_RATE_TAG_KEY, root_info.rate()));
                if let Some(rate_str) = format_sampling_rate(root_info.rate()) {
                    result.push(factory.create_string(
                        SAMPLING_KNUTH_RATE_TAG_KEY,
                        std::borrow::Cow::Owned(rate_str),
                    ));
                }
            }
            _ => {}
        }

        let priority = self.priority;
        result.push(factory.create_i64(SAMPLING_PRIORITY_TAG_KEY, priority.into_i8() as i64));

        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{
        attr::{ENV_TAG, RESOURCE_TAG},
        pattern,
    };
    use crate::types::{AttributeLike, TraceIdLike, ValueLike};
    use std::borrow::Cow;
    use std::collections::HashMap;

    // Test-only semantic convention constants
    const HTTP_REQUEST_METHOD: &str = "http.request.method";
    const SERVICE_NAME: &str = "service.name";

    // HTTP status code attribute constants (for tests)
    const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";
    const HTTP_STATUS_CODE: &str = "http.status_code";

    // ============================================================================
    // Test-only data structures
    // ============================================================================

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestTraceId {
        bytes: [u8; 16],
    }

    impl TestTraceId {
        fn from_bytes(bytes: [u8; 16]) -> Self {
            Self { bytes }
        }
    }

    impl TraceIdLike for TestTraceId {
        fn to_u128(&self) -> u128 {
            u128::from_be_bytes(self.bytes)
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum TestValue {
        String(String),
        I64(i64),
        F64(f64),
    }

    impl ValueLike for TestValue {
        fn extract_float(&self) -> Option<f64> {
            match self {
                TestValue::I64(i) => Some(*i as f64),
                TestValue::F64(f) => Some(*f),
                _ => None,
            }
        }

        fn extract_string(&self) -> Option<Cow<'_, str>> {
            match self {
                TestValue::String(s) => Some(Cow::Borrowed(s.as_str())),
                TestValue::I64(i) => Some(Cow::Owned(i.to_string())),
                TestValue::F64(f) => Some(Cow::Owned(f.to_string())),
            }
        }
    }

    #[derive(Clone, Debug)]
    struct TestAttribute {
        key: String,
        value: TestValue,
    }

    impl TestAttribute {
        fn new(key: impl Into<String>, value: impl Into<TestValue>) -> Self {
            Self {
                key: key.into(),
                value: value.into(),
            }
        }
    }

    impl AttributeLike for TestAttribute {
        type Value = TestValue;

        fn key(&self) -> &str {
            &self.key
        }

        fn value(&self) -> &Self::Value {
            &self.value
        }
    }

    impl From<&str> for TestValue {
        fn from(s: &str) -> Self {
            TestValue::String(s.to_string())
        }
    }

    impl From<String> for TestValue {
        fn from(s: String) -> Self {
            TestValue::String(s)
        }
    }

    struct TestSpan<'a> {
        name: &'a str,
        attributes: &'a [TestAttribute],
    }

    impl<'a> TestSpan<'a> {
        fn new(name: &'a str, attributes: &'a [TestAttribute]) -> Self {
            Self { name, attributes }
        }

        fn get_operation_name(&self) -> Cow<'_, str> {
            // Check for HTTP spans - label them all as client spans
            if self
                .attributes
                .iter()
                .any(|attr| attr.key() == HTTP_REQUEST_METHOD)
            {
                return Cow::Borrowed("http.client.request");
            }

            // Default fallback
            Cow::Borrowed("internal")
        }
    }

    impl<'a> SpanProperties for TestSpan<'a> {
        type Attribute = TestAttribute;

        fn operation_name(&self) -> Cow<'_, str> {
            self.get_operation_name()
        }

        fn service(&self) -> Cow<'_, str> {
            self.attributes
                .iter()
                .find(|attr| attr.key() == SERVICE_NAME)
                .and_then(|attr| attr.value().extract_string())
                .unwrap_or(Cow::Borrowed(""))
        }

        fn env(&self) -> Cow<'_, str> {
            self.attributes
                .iter()
                .find(|attr| attr.key() == "datadog.env" || attr.key() == ENV_TAG)
                .and_then(|attr| attr.value().extract_string())
                .unwrap_or(Cow::Borrowed(""))
        }

        fn resource(&self) -> Cow<'_, str> {
            self.attributes
                .iter()
                .find(|attr| attr.key() == RESOURCE_TAG)
                .and_then(|attr| attr.value().extract_string())
                .unwrap_or(Cow::Borrowed(self.name))
        }

        fn status_code(&self) -> Option<u32> {
            self.attributes
                .iter()
                .find(|attr| {
                    attr.key() == HTTP_RESPONSE_STATUS_CODE || attr.key() == HTTP_STATUS_CODE
                })
                .and_then(|attr| match attr.value() {
                    TestValue::I64(i) => Some(*i as u32),
                    _ => None,
                })
        }

        fn attributes<'b>(&'b self) -> impl Iterator<Item = &'b Self::Attribute>
        where
            Self: 'b,
        {
            self.attributes.iter()
        }

        fn get_alternate_key<'b>(&self, key: &'b str) -> Option<Cow<'b, str>> {
            match key {
                HTTP_RESPONSE_STATUS_CODE => Some(Cow::Borrowed(HTTP_STATUS_CODE)),
                HTTP_REQUEST_METHOD => Some(Cow::Borrowed("http.method")),
                _ => None,
            }
        }
    }

    struct TestSamplingData<'a> {
        is_parent_sampled: Option<bool>,
        trace_id: &'a TestTraceId,
        name: &'a str,
        attributes: &'a [TestAttribute],
    }

    impl<'a> TestSamplingData<'a> {
        fn new(
            is_parent_sampled: Option<bool>,
            trace_id: &'a TestTraceId,
            name: &'a str,
            attributes: &'a [TestAttribute],
        ) -> Self {
            Self {
                is_parent_sampled,
                trace_id,
                name,
                attributes,
            }
        }
    }

    impl<'a> SamplingData for TestSamplingData<'a> {
        type TraceId = TestTraceId;
        type Properties<'b>
            = TestSpan<'b>
        where
            Self: 'b;

        fn is_parent_sampled(&self) -> Option<bool> {
            self.is_parent_sampled
        }

        fn trace_id(&self) -> &Self::TraceId {
            self.trace_id
        }

        fn with_span_properties<S, T, F>(&self, s: &S, f: F) -> T
        where
            F: for<'b> Fn(&S, &TestSpan<'b>) -> T,
        {
            let span = TestSpan::new(self.name, self.attributes);
            f(s, &span)
        }
    }

    struct TestAttributeFactory;

    impl crate::types::AttributeFactory for TestAttributeFactory {
        type Attribute = TestAttribute;

        fn create_i64(&self, key: &'static str, value: i64) -> Self::Attribute {
            TestAttribute::new(key, TestValue::I64(value))
        }

        fn create_f64(&self, key: &'static str, value: f64) -> Self::Attribute {
            TestAttribute::new(key, TestValue::F64(value))
        }

        fn create_string(&self, key: &'static str, value: Cow<'static, str>) -> Self::Attribute {
            TestAttribute::new(key, TestValue::String(value.into_owned()))
        }
    }

    // ============================================================================
    // Test helper functions
    // ============================================================================

    // Helper function to create a trace ID
    fn create_trace_id() -> TestTraceId {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        TestTraceId::from_bytes(bytes)
    }

    // Helper function to create attributes for testing (with resource and env)
    fn create_attributes(resource: &'static str, env: &'static str) -> Vec<TestAttribute> {
        vec![
            TestAttribute::new(RESOURCE_TAG, resource),
            TestAttribute::new("datadog.env", env),
        ]
    }

    // Helper function to create attributes with service
    fn create_attributes_with_service(
        service: String,
        resource: &'static str,
        env: &'static str,
    ) -> Vec<TestAttribute> {
        vec![
            TestAttribute::new(SERVICE_NAME, service),
            TestAttribute::new(RESOURCE_TAG, resource),
            TestAttribute::new("datadog.env", env),
        ]
    }

    // Helper function to create SamplingData for testing
    fn create_sampling_data<'a>(
        is_parent_sampled: Option<bool>,
        trace_id: &'a TestTraceId,
        name: &'a str,
        attributes: &'a [TestAttribute],
    ) -> TestSamplingData<'a> {
        TestSamplingData::new(is_parent_sampled, trace_id, name, attributes)
    }

    #[test]
    fn test_sampling_rule_creation() {
        let rule = SamplingRule::new(
            0.5,
            Some("test-service".to_string()),
            Some("test-name".to_string()),
            Some("test-resource".to_string()),
            Some(HashMap::from([(
                "custom-tag".to_string(),
                "tag-value".to_string(),
            )])),
            Some("customer".to_string()),
        );

        assert_eq!(rule.sample_rate, 0.5);
        assert_eq!(rule.service_matcher.unwrap().pattern(), "test-service");
        assert_eq!(rule.name_matcher.unwrap().pattern(), "test-name");
        assert_eq!(
            rule.resource_matcher.unwrap().pattern(),
            "test-resource".to_string()
        );
        assert_eq!(
            rule.tag_matchers.get("custom-tag").unwrap().pattern(),
            "tag-value"
        );
        assert_eq!(rule.provenance, "customer");
    }

    #[test]
    fn test_sampling_rule_with_no_rule() {
        // Create a rule without specifying any criteria
        let rule = SamplingRule::new(
            0.5, None, // No service
            None, // No name
            None, // No resource
            None, // No tags
            None, // Default provenance
        );

        // Verify fields are set to None or empty
        assert_eq!(rule.sample_rate, 0.5);
        assert!(rule.service_matcher.is_none());
        assert!(rule.name_matcher.is_none());
        assert!(rule.resource_matcher.is_none());
        assert!(rule.tag_matchers.is_empty());
        assert_eq!(rule.provenance, "default");

        // Verify no matchers were created
        assert!(rule.service_matcher.is_none());
        assert!(rule.name_matcher.is_none());
        assert!(rule.resource_matcher.is_none());
        assert!(rule.tag_matchers.is_empty());

        // Test that a rule with NO_RULE constants behaves the same as None
        let rule_with_empty_strings = SamplingRule::new(
            0.5,
            Some(pattern::NO_RULE.to_string()), // Empty service string
            Some(pattern::NO_RULE.to_string()), // Empty name string
            Some(pattern::NO_RULE.to_string()), // Empty resource string
            Some(HashMap::from([(
                pattern::NO_RULE.to_string(),
                pattern::NO_RULE.to_string(),
            )])), // Empty tag
            None,
        );

        // Verify that matchers aren't created for NO_RULE values
        assert!(rule_with_empty_strings.service_matcher.is_none());
        assert!(rule_with_empty_strings.name_matcher.is_none());
        assert!(rule_with_empty_strings.resource_matcher.is_none());
        assert!(rule_with_empty_strings.tag_matchers.is_empty());

        // Create a span with some attributes
        let attributes = create_attributes("some-resource", "some-env");

        // Both rules should match any span since they have no criteria
        let span = TestSpan::new("", &attributes);
        assert!(rule.matches(&span));
        assert!(rule_with_empty_strings.matches(&span));
    }

    #[test]
    fn test_sampling_rule_matches() {
        // Create a rule with specific service and name patterns
        let _rule = SamplingRule::new(
            0.5,
            Some("web-*".to_string()),
            Some("http.*".to_string()),
            None,
            Some(HashMap::from([(
                "custom_key".to_string(),
                "custom_value".to_string(),
            )])),
            None,
        );
    }

    #[test]
    fn test_sample_method() {
        // Create two rules with different rates
        let rule_always = SamplingRule::new(1.0, None, None, None, None, None);
        let rule_never = SamplingRule::new(0.0, None, None, None, None, None);

        let trace_id = create_trace_id();

        // Rule with rate 1.0 should always sample
        assert!(rule_always.sample(&trace_id));

        // Rule with rate 0.0 should never sample
        assert!(!rule_never.sample(&trace_id));
    }

    #[test]
    fn test_datadog_sampler_creation() {
        // Create a sampler with default config
        let sampler = DatadogSampler::new(vec![], 100);
        assert!(sampler.rules.is_empty());
        assert!(sampler.service_samplers.is_empty());

        // Create a sampler with rules
        let rule = SamplingRule::new(0.5, None, None, None, None, None);
        let sampler_with_rules = DatadogSampler::new(vec![rule], 200);
        assert_eq!(sampler_with_rules.rules.len(), 1);
    }

    #[test]
    fn test_service_key_generation() {
        let test_service_name = "test-service".to_string();
        let sampler = DatadogSampler::new(vec![], 100);

        // Test with service and env
        let attrs =
            create_attributes_with_service(test_service_name.clone(), "resource", "production");
        let span = TestSpan::new("test-span", attrs.as_slice());
        assert_eq!(
            sampler.service_key(&span),
            format!("service:{test_service_name},env:production")
        );

        // Test with missing env
        let attrs_no_env = vec![
            TestAttribute::new(SERVICE_NAME, test_service_name.clone()),
            TestAttribute::new(RESOURCE_TAG, "resource"),
        ];
        let span = TestSpan::new("test-span", attrs_no_env.as_slice());
        assert_eq!(
            sampler.service_key(&span),
            format!("service:{test_service_name},env:")
        );
    }

    #[test]
    fn test_update_service_rates() {
        let sampler = DatadogSampler::new(vec![], 100);

        // Update with service rates
        let mut rates = HashMap::new();
        rates.insert("service:web,env:prod".to_string(), 0.5);
        rates.insert("service:api,env:prod".to_string(), 0.75);

        sampler.service_samplers.update_rates(rates);

        // Check number of samplers
        assert_eq!(sampler.service_samplers.len(), 2);

        // Verify keys exist
        assert!(sampler
            .service_samplers
            .contains_key("service:web,env:prod"));
        assert!(sampler
            .service_samplers
            .contains_key("service:api,env:prod"));

        // Verify the sampling rates are correctly set
        if let Some(web_sampler) = sampler.service_samplers.get("service:web,env:prod") {
            assert_eq!(web_sampler.sample_rate(), 0.5);
        } else {
            panic!("Web service sampler not found");
        }

        if let Some(api_sampler) = sampler.service_samplers.get("service:api,env:prod") {
            assert_eq!(api_sampler.sample_rate(), 0.75);
        } else {
            panic!("API service sampler not found");
        }
    }

    #[test]
    fn test_find_matching_rule() {
        // Create rules with different priorities and service matchers
        let rule1 = SamplingRule::new(
            0.1,
            Some("service1".to_string()),
            None,
            None,
            None,
            Some("customer".to_string()), // Highest priority
        );

        let rule2 = SamplingRule::new(
            0.2,
            Some("service2".to_string()),
            None,
            None,
            None,
            Some("dynamic".to_string()), // Middle priority
        );

        let rule3 = SamplingRule::new(
            0.3,
            Some("service*".to_string()), // Wildcard service
            None,
            None,
            None,
            Some("default".to_string()), // Lowest priority
        );

        let sampler = DatadogSampler::new(vec![rule1.clone(), rule2.clone(), rule3.clone()], 100);

        // Test with a specific service that should match the first rule (rule1)
        {
            let attrs1 = create_attributes_with_service(
                "service1".to_string(),
                "resource_val_for_attr1",
                "prod",
            );
            let span = TestSpan::new("test-span", attrs1.as_slice());
            let matching_rule_for_attrs1 = sampler.find_matching_rule(&span);
            assert!(
                matching_rule_for_attrs1.is_some(),
                "Expected rule1 to match for service1"
            );
            let rule = matching_rule_for_attrs1.unwrap();
            assert_eq!(rule.sample_rate, 0.1, "Expected rule1 sample rate");
            assert_eq!(rule.provenance, "customer", "Expected rule1 provenance");
        }

        // Test with a specific service that should match the second rule (rule2)
        {
            let attrs2 = create_attributes_with_service(
                "service2".to_string(),
                "resource_val_for_attr2",
                "prod",
            );
            let span = TestSpan::new("test-span", attrs2.as_slice());
            let matching_rule_for_attrs2 = sampler.find_matching_rule(&span);
            assert!(
                matching_rule_for_attrs2.is_some(),
                "Expected rule2 to match for service2"
            );
            let rule = matching_rule_for_attrs2.unwrap();
            assert_eq!(rule.sample_rate, 0.2, "Expected rule2 sample rate");
            assert_eq!(rule.provenance, "dynamic", "Expected rule2 provenance");
        }

        // Test with a service that matches the wildcard rule (rule3)
        {
            let attrs3 = create_attributes_with_service(
                "service3".to_string(),
                "resource_val_for_attr3",
                "prod",
            );
            let span = TestSpan::new("test-span", attrs3.as_slice());
            let matching_rule_for_attrs3 = sampler.find_matching_rule(&span);
            assert!(
                matching_rule_for_attrs3.is_some(),
                "Expected rule3 to match for service3"
            );
            let rule = matching_rule_for_attrs3.unwrap();
            assert_eq!(rule.sample_rate, 0.3, "Expected rule3 sample rate");
            assert_eq!(rule.provenance, "default", "Expected rule3 provenance");
        }

        // Test with a service that doesn't match any rule's service pattern
        {
            let attrs4 = create_attributes_with_service(
                "other_sampler_service".to_string(),
                "resource_val_for_attr4",
                "prod",
            );
            let span = TestSpan::new("test-span", attrs4.as_slice());
            let matching_rule_for_attrs4 = sampler.find_matching_rule(&span);
            assert!(
                matching_rule_for_attrs4.is_none(),
                "Expected no rule to match for service 'other_sampler_service'"
            );
        }
    }

    #[test]
    fn test_get_sampling_mechanism() {
        let sampler = DatadogSampler::new(vec![], 100);

        // Create rules with different provenances
        let rule_customer =
            SamplingRule::new(0.1, None, None, None, None, Some("customer".to_string()));
        let rule_dynamic =
            SamplingRule::new(0.2, None, None, None, None, Some("dynamic".to_string()));
        let rule_default =
            SamplingRule::new(0.3, None, None, None, None, Some("default".to_string()));

        // Test with customer rule
        let mechanism1 = sampler.get_sampling_mechanism(Some(&rule_customer), false);
        assert_eq!(mechanism1, mechanism::REMOTE_USER_TRACE_SAMPLING_RULE);

        // Test with dynamic rule
        let mechanism2 = sampler.get_sampling_mechanism(Some(&rule_dynamic), false);
        assert_eq!(mechanism2, mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE);

        // Test with default rule
        let mechanism3 = sampler.get_sampling_mechanism(Some(&rule_default), false);
        assert_eq!(mechanism3, mechanism::LOCAL_USER_TRACE_SAMPLING_RULE);

        // Test with agent sampler
        let mechanism4 = sampler.get_sampling_mechanism(None, true);
        assert_eq!(mechanism4, mechanism::AGENT_RATE_BY_SERVICE);

        // Test fallback case
        let mechanism5 = sampler.get_sampling_mechanism(None, false);
        assert_eq!(mechanism5, mechanism::DEFAULT);
    }

    #[test]
    fn test_add_dd_sampling_tags() {
        // Test with RecordAndSample decision and LocalUserTraceSamplingRule mechanism
        let sample_rate = 0.5;
        let is_sampled = true;
        let mechanism = mechanism::LOCAL_USER_TRACE_SAMPLING_RULE;
        let sampling_result = DdSamplingResult {
            priority: mechanism.to_priority(is_sampled),
            trace_root_info: Some(TraceRootSamplingInfo {
                mechanism,
                rate: 0.5,
                rl_effective_rate: None,
            }),
        };

        let attrs = sampling_result
            .to_dd_sampling_tags(&TestAttributeFactory)
            .unwrap_or_default();

        // Verify the number of attributes (decision_maker + priority + rule_rate + ksr)
        assert_eq!(attrs.len(), 4);

        // Check individual attributes
        let mut found_decision_maker = false;
        let mut found_priority = false;
        let mut found_rule_rate = false;
        let mut found_ksr = false;

        for attr in &attrs {
            match attr.key() {
                SAMPLING_DECISION_MAKER_TAG_KEY => {
                    let value_str = match attr.value() {
                        TestValue::String(s) => s.to_string(),
                        _ => panic!("Expected string value for decision maker tag"),
                    };
                    assert_eq!(value_str, mechanism.to_cow());
                    found_decision_maker = true;
                }
                SAMPLING_PRIORITY_TAG_KEY => {
                    // For LocalUserTraceSamplingRule with KEEP, it should be USER_KEEP
                    let expected_priority = mechanism.to_priority(true).into_i8() as i64;

                    let value_int = match attr.value() {
                        TestValue::I64(i) => *i,
                        _ => panic!("Expected integer value for priority tag"),
                    };
                    assert_eq!(value_int, expected_priority);
                    found_priority = true;
                }
                SAMPLING_RULE_RATE_TAG_KEY => {
                    let value_float = match attr.value() {
                        TestValue::F64(f) => *f,
                        _ => panic!("Expected float value for rule rate tag"),
                    };
                    assert_eq!(value_float, sample_rate);
                    found_rule_rate = true;
                }
                SAMPLING_KNUTH_RATE_TAG_KEY => {
                    let value_str = match attr.value() {
                        TestValue::String(s) => s.to_string(),
                        _ => panic!("Expected string value for ksr tag"),
                    };
                    assert_eq!(value_str, "0.5");
                    found_ksr = true;
                }
                _ => {}
            }
        }

        assert!(found_decision_maker, "Missing decision maker tag");
        assert!(found_priority, "Missing priority tag");
        assert!(found_rule_rate, "Missing rule rate tag");
        assert!(found_ksr, "Missing knuth sampling rate tag");

        // Test with rate limiting
        let rate_limit = 0.5;
        let is_sampled = false;
        let mechanism = mechanism::LOCAL_USER_TRACE_SAMPLING_RULE;
        let sampling_result = DdSamplingResult {
            priority: mechanism.to_priority(is_sampled),
            trace_root_info: Some(TraceRootSamplingInfo {
                mechanism,
                rate: 0.5,
                rl_effective_rate: Some(rate_limit),
            }),
        };
        let attrs_with_limit = sampling_result
            .to_dd_sampling_tags(&TestAttributeFactory)
            .unwrap_or_default();

        // With rate limiting, there should be one more attribute
        assert_eq!(attrs_with_limit.len(), 5);

        // Check for rate limit attribute
        let mut found_limit = false;
        for attr in &attrs_with_limit {
            if attr.key() == RL_EFFECTIVE_RATE {
                let value_float = match attr.value() {
                    TestValue::F64(f) => *f,
                    _ => panic!("Expected float value for rate limit tag"),
                };
                assert_eq!(value_float, rate_limit);
                found_limit = true;
                break;
            }
        }

        assert!(found_limit, "Missing rate limit tag");

        // Test with AgentRateByService mechanism to check for SAMPLING_AGENT_RATE_TAG_KEY

        let agent_rate = 0.75;
        let is_sampled = false;
        let mechanism = mechanism::AGENT_RATE_BY_SERVICE;
        let sampling_result = DdSamplingResult {
            priority: mechanism.to_priority(is_sampled),
            trace_root_info: Some(TraceRootSamplingInfo {
                mechanism,
                rate: agent_rate,
                rl_effective_rate: None,
            }),
        };

        let agent_attrs = sampling_result
            .to_dd_sampling_tags(&TestAttributeFactory)
            .unwrap_or_default();

        // Verify the number of attributes (should be 4: decision_maker + priority +
        // agent_rate + ksr)
        assert_eq!(agent_attrs.len(), 4);

        // Check for agent rate tag and ksr tag
        let mut found_agent_rate = false;
        let mut found_ksr = false;
        for attr in &agent_attrs {
            match attr.key() {
                SAMPLING_AGENT_RATE_TAG_KEY => {
                    let value_float = match attr.value() {
                        TestValue::F64(f) => *f,
                        _ => panic!("Expected float value for agent rate tag"),
                    };
                    assert_eq!(value_float, agent_rate);
                    found_agent_rate = true;
                }
                SAMPLING_KNUTH_RATE_TAG_KEY => {
                    let value_str = match attr.value() {
                        TestValue::String(s) => s.to_string(),
                        _ => panic!("Expected string value for ksr tag"),
                    };
                    assert_eq!(value_str, "0.75");
                    found_ksr = true;
                }
                _ => {}
            }
        }

        assert!(found_agent_rate, "Missing agent rate tag");
        assert!(
            found_ksr,
            "Missing knuth sampling rate tag for agent mechanism"
        );

        // Also check that the SAMPLING_RULE_RATE_TAG_KEY is NOT present for agent mechanism
        for attr in &agent_attrs {
            assert_ne!(
                attr.key(),
                SAMPLING_RULE_RATE_TAG_KEY,
                "Rule rate tag should not be present for agent mechanism"
            );
        }
    }

    #[test]
    fn test_format_sampling_rate() {
        // Exact values
        assert_eq!(format_sampling_rate(1.0), Some("1".to_string()));
        assert_eq!(format_sampling_rate(0.5), Some("0.5".to_string()));
        assert_eq!(format_sampling_rate(0.1), Some("0.1".to_string()));
        assert_eq!(format_sampling_rate(0.0), Some("0".to_string()));

        // Trailing zeros should be stripped
        assert_eq!(format_sampling_rate(0.100000), Some("0.1".to_string()));
        assert_eq!(format_sampling_rate(0.500000), Some("0.5".to_string()));

        // Truncation to 6 significant digits
        assert_eq!(
            format_sampling_rate(0.7654321),
            Some("0.765432".to_string())
        );
        assert_eq!(
            format_sampling_rate(0.123456789),
            Some("0.123457".to_string())
        );

        // Small values
        assert_eq!(format_sampling_rate(0.001), Some("0.001".to_string()));

        // Boundary values
        assert_eq!(format_sampling_rate(0.75), Some("0.75".to_string()));
        assert_eq!(format_sampling_rate(0.999999), Some("0.999999".to_string()));

        // Invalid rates
        assert_eq!(format_sampling_rate(-0.1), None);
        assert_eq!(format_sampling_rate(1.1), None);
        assert_eq!(format_sampling_rate(f64::NAN), None);
        assert_eq!(format_sampling_rate(f64::INFINITY), None);
        assert_eq!(format_sampling_rate(f64::NEG_INFINITY), None);
    }

    #[test]
    fn test_should_sample_parent_context() {
        let sampler = DatadogSampler::new(vec![], 100);

        // Create empty slices for attributes and links
        let empty_attrs: &[TestAttribute] = &[];
        let trace_id = create_trace_id();

        // Test with sampled parent context
        let data_sampled = create_sampling_data(Some(true), &trace_id, "span", empty_attrs);
        let result_sampled = sampler.sample(&data_sampled);

        // Should inherit the sampling decision from parent
        assert!(result_sampled.get_priority().is_keep());
        assert!(result_sampled
            .to_dd_sampling_tags(&TestAttributeFactory)
            .is_none());

        // Test with non-sampled parent context
        let data_not_sampled = create_sampling_data(Some(false), &trace_id, "span", empty_attrs);
        let result_not_sampled = sampler.sample(&data_not_sampled);

        // Should inherit the sampling decision from parent
        assert!(!result_not_sampled.get_priority().is_keep());
        assert!(result_not_sampled
            .to_dd_sampling_tags(&TestAttributeFactory)
            .is_none());
    }

    #[test]
    fn test_should_sample_with_rule() {
        // Create a rule that always samples
        let rule = SamplingRule::new(
            1.0,
            Some("test-service".to_string()),
            None,
            None,
            None,
            None,
        );

        let sampler = DatadogSampler::new(vec![rule], 100);

        let trace_id = create_trace_id();

        // Test with matching attributes
        let attrs = create_attributes("resource", "prod");
        let data = create_sampling_data(None, &trace_id, "span", attrs.as_slice());
        let result = sampler.sample(&data);

        // Should sample and add attributes
        assert!(result.get_priority().is_keep());
        assert!(result.to_dd_sampling_tags(&TestAttributeFactory).is_some());

        // Test with non-matching attributes
        let attrs_no_match = create_attributes("other-resource", "prod");
        let data_no_match =
            create_sampling_data(None, &trace_id, "span", attrs_no_match.as_slice());
        let result_no_match = sampler.sample(&data_no_match);

        // Should still sample (default behavior when no rules match) and add attributes
        assert!(result_no_match.get_priority().is_keep());
        assert!(result_no_match
            .to_dd_sampling_tags(&TestAttributeFactory)
            .is_some());
    }

    #[test]
    fn test_should_sample_with_service_rates() {
        // Initialize sampler
        let sampler = DatadogSampler::new(vec![], 100);

        // Add service rates for different service+env combinations
        let mut rates = HashMap::new();
        rates.insert("service:test-service,env:prod".to_string(), 1.0); // Always sample for test-service in prod
        rates.insert("service:other-service,env:prod".to_string(), 0.0); // Never sample for other-service in prod

        sampler.update_service_rates(rates);

        let trace_id = create_trace_id();

        // Test with attributes that should lead to "service:test-service,env:prod" key
        let attrs_sample = create_attributes_with_service(
            "test-service".to_string(),
            "any_resource_name_matching_env",
            "prod",
        );
        let data_sample = create_sampling_data(
            None,
            &trace_id,
            "span_for_test_service",
            attrs_sample.as_slice(),
        );
        let result_sample = sampler.sample(&data_sample);
        // Expect RecordAndSample because service_key will be "service:test-service,env:prod" ->
        // rate 1.0
        assert!(
            result_sample.get_priority().is_keep(),
            "Span for test-service/prod should be sampled"
        );

        // Test with attributes that should lead to "service:other-service,env:prod" key
        let attrs_no_sample = create_attributes_with_service(
            "other-service".to_string(),
            "any_resource_name_matching_env",
            "prod",
        );
        let data_no_sample = create_sampling_data(
            None,
            &trace_id,
            "span_for_other_service",
            attrs_no_sample.as_slice(),
        );
        let result_no_sample = sampler.sample(&data_no_sample);
        // Expect Drop because service_key will be "service:other-service,env:prod" -> rate 0.0
        assert!(
            !result_no_sample.get_priority().is_keep(),
            "Span for other-service/prod should be dropped"
        );
    }

    #[test]
    fn test_sampling_rule_matches_float_attributes() {
        // Helper to create attributes with a float value
        fn create_attributes_with_float(
            tag_key: &'static str,
            float_value: f64,
        ) -> Vec<TestAttribute> {
            vec![
                TestAttribute::new(RESOURCE_TAG, "resource"),
                TestAttribute::new(ENV_TAG, "prod"),
                TestAttribute::new(tag_key, TestValue::F64(float_value)),
            ]
        }

        // Test case 1: Rule with exact value matching integer float
        let rule_integer = SamplingRule::new(
            0.5,
            None,
            None,
            None,
            Some(HashMap::from([("float_tag".to_string(), "42".to_string())])),
            None,
        );

        // Should match integer float
        let integer_float_attrs = create_attributes_with_float("float_tag", 42.0);
        let span = TestSpan::new("test-span", integer_float_attrs.as_slice());
        assert!(rule_integer.matches(&span));

        // Test case 2: Rule with wildcard pattern and non-integer float
        let rule_wildcard = SamplingRule::new(
            0.5,
            None,
            None,
            None,
            Some(HashMap::from([("float_tag".to_string(), "*".to_string())])),
            None,
        );

        // Should match non-integer float with wildcard pattern
        let decimal_float_attrs = create_attributes_with_float("float_tag", 42.5);
        let span = TestSpan::new("test-span", decimal_float_attrs.as_slice());
        assert!(rule_wildcard.matches(&span));

        // Test case 3: Rule with specific pattern and non-integer float
        // With our simplified logic, non-integer floats will never match non-wildcard patterns
        let rule_specific = SamplingRule::new(
            0.5,
            None,
            None,
            None,
            Some(HashMap::from([(
                "float_tag".to_string(),
                "42.5".to_string(),
            )])),
            None,
        );

        // Should NOT match the exact decimal value because non-integer floats only match wildcards
        let decimal_float_attrs = create_attributes_with_float("float_tag", 42.5);
        let span = TestSpan::new("test-span", decimal_float_attrs.as_slice());
        assert!(!rule_specific.matches(&span));
        // Test case 4: Pattern with partial wildcard '*' for suffix
        let rule_prefix = SamplingRule::new(
            0.5,
            None,
            None,
            None,
            Some(HashMap::from([(
                "float_tag".to_string(),
                "42.*".to_string(),
            )])),
            None,
        );

        // Should NOT match decimal values as we don't do partial pattern matching for non-integer
        // floats
        let span = TestSpan::new("test-span", decimal_float_attrs.as_slice());
        assert!(!rule_prefix.matches(&span));
    }

    #[test]
    fn test_operation_name() {
        // Test that the sampler correctly matches rules based on operation names
        // Operation name generation itself is tested in otel_mappings unit tests

        let http_rule = SamplingRule::new(
            1.0,
            None,
            Some("http.*.request".to_string()),
            None,
            None,
            Some("default".to_string()),
        );

        let sampler = DatadogSampler::new(vec![http_rule], 100);

        let trace_id = create_trace_id();

        // HTTP client request should match http_rule (operation name: http.client.request)
        let http_client_attrs = vec![TestAttribute::new(HTTP_REQUEST_METHOD, "GET")];
        let data = create_sampling_data(None, &trace_id, "test-span", &http_client_attrs);
        assert!(sampler.sample(&data).get_priority().is_keep());

        // Span that doesn't match the rule should still be sampled (default behavior)
        let internal_attrs = vec![TestAttribute::new("custom.tag", "value")];
        let data = create_sampling_data(None, &trace_id, "test-span", &internal_attrs);
        assert!(sampler.sample(&data).get_priority().is_keep());
    }

    #[test]
    fn test_on_rules_update_callback() {
        // Create a sampler with initial rules
        let initial_rule = SamplingRule::new(
            0.1,
            Some("initial-service".to_string()),
            None,
            None,
            None,
            Some("default".to_string()),
        );

        let sampler = DatadogSampler::new(vec![initial_rule], 100);

        // Verify initial state
        assert_eq!(sampler.rules.len(), 1);

        // Get the callback
        let callback = sampler.on_rules_update();

        // Create new rules directly as SamplingRuleConfig objects
        let new_rules = vec![
            SamplingRuleConfig {
                sample_rate: 0.5,
                service: Some("web-*".to_string()),
                name: Some("http.*".to_string()),
                resource: None,
                tags: std::collections::HashMap::new(),
                provenance: "customer".to_string(),
            },
            SamplingRuleConfig {
                sample_rate: 0.2,
                service: Some("api-*".to_string()),
                name: None,
                resource: Some("/api/*".to_string()),
                tags: [("env".to_string(), "prod".to_string())].into(),
                provenance: "dynamic".to_string(),
            },
        ];

        // Apply the update
        callback(&new_rules);

        // Verify the rules were updated
        assert_eq!(sampler.rules.len(), 2);

        // Test that the new rules work by finding a matching rule
        // Create attributes that will generate an operation name matching "http.*"
        // and service matching "web-*"
        let attrs = vec![
            TestAttribute::new(SERVICE_NAME, "web-frontend"),
            TestAttribute::new(HTTP_REQUEST_METHOD, "GET"), /* This will make operation name
                                                             * "http.client.request" */
        ];
        let span = TestSpan::new("test-span", attrs.as_slice());

        let matching_rule = sampler.find_matching_rule(&span);
        assert!(matching_rule.is_some(), "Expected to find a matching rule for service 'web-frontend' and name 'http.client.request'");
        let rule = matching_rule.unwrap();
        assert_eq!(rule.sample_rate, 0.5);
        assert_eq!(rule.provenance, "customer");

        // Test with empty rules array
        callback(&[]);
        assert_eq!(sampler.rules.len(), 0); // Should now have no rules
    }
}
