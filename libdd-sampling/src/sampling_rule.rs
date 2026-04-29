// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::constants::pattern::NO_RULE;
use crate::glob_matcher::GlobMatcher;
use crate::rate_sampler::RateSampler;
use crate::sampling_rule_config::SamplingRuleConfig;
use crate::types::{AttributeLike, SpanProperties, TraceIdLike, ValueLike};
use std::collections::HashMap;

// HTTP status code attribute constants
const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";
const HTTP_STATUS_CODE: &str = "http.status_code";

fn matcher_from_rule(rule: &str) -> Option<GlobMatcher> {
    (rule != NO_RULE).then(|| GlobMatcher::new(rule))
}

/// Represents a sampling rule with criteria for matching spans
#[derive(Clone, Debug)]
pub struct SamplingRule {
    /// The sample rate to apply when this rule matches (0.0-1.0)
    pub(crate) sample_rate: f64,

    /// Where this rule comes from (customer, dynamic, default)
    pub(crate) provenance: String,

    /// Internal rate sampler used when this rule matches
    rate_sampler: RateSampler,

    /// Glob matchers for pattern matching
    pub(crate) name_matcher: Option<GlobMatcher>,
    pub(crate) service_matcher: Option<GlobMatcher>,
    pub(crate) resource_matcher: Option<GlobMatcher>,
    pub(crate) tag_matchers: HashMap<String, GlobMatcher>,
}

impl SamplingRule {
    /// Converts a vector of SamplingRuleConfig into SamplingRule objects
    /// Centralizes the conversion logic
    pub fn from_configs(configs: Vec<SamplingRuleConfig>) -> Vec<Self> {
        configs
            .into_iter()
            .map(|config| {
                Self::new(
                    config.sample_rate,
                    config.service,
                    config.name,
                    config.resource,
                    Some(config.tags),
                    Some(config.provenance),
                )
            })
            .collect()
    }

    /// Creates a new sampling rule
    pub fn new(
        sample_rate: f64,
        service: Option<String>,
        name: Option<String>,
        resource: Option<String>,
        tags: Option<HashMap<String, String>>,
        provenance: Option<String>,
    ) -> Self {
        // Create glob matchers for the patterns
        let name_matcher = name.as_deref().and_then(matcher_from_rule);
        let service_matcher = service.as_deref().and_then(matcher_from_rule);
        let resource_matcher = resource.as_deref().and_then(matcher_from_rule);

        // Create matchers for tag values
        let tag_map = tags.clone().unwrap_or_default();
        let mut tag_matchers = HashMap::with_capacity(tag_map.len());
        for (key, value) in &tag_map {
            if let Some(matcher) = matcher_from_rule(value) {
                tag_matchers.insert(key.clone(), matcher);
            }
        }

        SamplingRule {
            sample_rate,
            provenance: provenance.unwrap_or_else(|| "default".to_string()),
            rate_sampler: RateSampler::new(sample_rate),
            name_matcher,
            service_matcher,
            resource_matcher,
            tag_matchers,
        }
    }

    /// Checks if this rule matches the given span's attributes and name
    /// The name is derived from the attributes and span kind
    pub(crate) fn matches(&self, span: &impl SpanProperties) -> bool {
        // Get the operation name from the span
        let name = span.operation_name();

        // Check name using glob matcher if specified
        if let Some(ref matcher) = self.name_matcher {
            if !matcher.matches(name.as_ref()) {
                return false;
            }
        }

        // Check service if specified using glob matcher
        if let Some(ref matcher) = self.service_matcher {
            // Get service from the span
            let service = span.service();

            // Match against the service
            if !matcher.matches(&service) {
                return false;
            }
        }

        // Get the resource string for matching
        let resource_str = span.resource();

        // Check resource if specified using glob matcher
        if let Some(ref matcher) = self.resource_matcher {
            // Use the resource from the span
            if !matcher.matches(resource_str.as_ref()) {
                return false;
            }
        }

        // Check all tags using glob matchers
        for (key, matcher) in &self.tag_matchers {
            let rule_tag_key_str = key.as_str();

            // Special handling for rules defined with "http.status_code" or
            // "http.response.status_code"
            if rule_tag_key_str == HTTP_STATUS_CODE || rule_tag_key_str == HTTP_RESPONSE_STATUS_CODE
            {
                match self.match_http_status_code_rule(matcher, span) {
                    Some(true) => continue,             // Status code matched
                    Some(false) | None => return false, // Status code didn't match or wasn't found
                }
            } else {
                // Logic for other tags:
                // First, try to match directly with the provided tag key
                let direct_match = span
                    .attributes()
                    .find(|attr| attr.key() == rule_tag_key_str)
                    .and_then(|attr| self.match_attribute_value(attr.value(), matcher));

                if direct_match.unwrap_or(false) {
                    continue;
                }

                // If no direct match, try to find the corresponding OpenTelemetry attribute that
                // maps to the Datadog tag key This handles cases where the rule key
                // is a Datadog key (e.g., "http.method") and the attribute is an
                // OTel key (e.g., "http.request.method")
                if rule_tag_key_str.starts_with("http.") {
                    let tag_match = span.attributes().any(|attr| {
                        if let Some(alternate_key) = span.get_alternate_key(attr.key()) {
                            if alternate_key == rule_tag_key_str {
                                return self
                                    .match_attribute_value(attr.value(), matcher)
                                    .unwrap_or(false);
                            }
                        }
                        false
                    });

                    if !tag_match {
                        return false; // Mapped attribute not found or did not match
                    }
                    // If tag_match is true, loop continues to next rule_tag_key.
                } else {
                    // For non-HTTP attributes, if we don't have a direct match, the rule doesn't
                    // match
                    return false;
                }
            }
        }

        true
    }

    /// Helper method to specifically match a rule against an HTTP status code extracted from
    /// attributes. Returns Some(true) if status code found and matches, Some(false) if found
    /// but not matched, None if not found.
    fn match_http_status_code_rule(
        &self,
        matcher: &GlobMatcher,
        span: &impl SpanProperties,
    ) -> Option<bool> {
        span.status_code().and_then(|status_code| {
            let status_value = ValueI64(i64::from(status_code));
            self.match_attribute_value(&status_value, matcher)
        })
    }

    // Helper method to match attribute values considering different value types
    fn match_attribute_value(&self, value: &impl ValueLike, matcher: &GlobMatcher) -> Option<bool> {
        // Floating point values are handled with special rules
        if let Some(float_val) = value.extract_float() {
            // Check if the float has a non-zero decimal part
            let has_decimal = float_val != (float_val as i64) as f64;

            // For non-integer floats, only match if it's a wildcard pattern
            if has_decimal {
                // All '*' pattern returns true, any other pattern returns false
                return Some(matcher.pattern().chars().all(|c| c == '*'));
            }

            // For integer floats, convert to string for matching
            return Some(matcher.matches(&float_val.to_string()));
        }

        // For non-float values, use normal matching
        value
            .extract_string()
            .map(|string_value| matcher.matches(&string_value))
    }

    /// Samples a trace ID using this rule's sample rate
    pub fn sample(&self, trace_id: &impl TraceIdLike) -> bool {
        // Delegate to the internal rate sampler's new sample method
        self.rate_sampler.sample(trace_id)
    }
}

/// Represents a priority for sampling rules
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleProvenance {
    Customer = 0,
    Dynamic = 1,
    Default = 2,
}

impl From<&str> for RuleProvenance {
    fn from(s: &str) -> Self {
        match s {
            "customer" => RuleProvenance::Customer,
            "dynamic" => RuleProvenance::Dynamic,
            _ => RuleProvenance::Default,
        }
    }
}

/// Helper struct for representing i64 values as ValueLike
struct ValueI64(i64);

impl ValueLike for ValueI64 {
    fn extract_float(&self) -> Option<f64> {
        Some(self.0 as f64)
    }

    fn extract_string(&self) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Owned(self.0.to_string()))
    }
}
