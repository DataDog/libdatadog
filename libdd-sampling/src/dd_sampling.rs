// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sampling types and mechanisms for Datadog distributed tracing.
//!
//! This module provides types for representing sampling decisions, priorities,
//! and mechanisms used in Datadog's trace sampling system.

use std::{borrow::Cow, fmt, str::FromStr};

/// Represents a sampling decision for a trace.
///
/// Contains the priority level and the mechanism that made the decision.
#[derive(Clone, Copy, Debug)]
pub struct SamplingDecision {
    /// The sampling priority indicating whether the trace should be kept or rejected.
    pub priority: Option<SamplingPriority>,
    /// The mechanism that made the sampling decision.
    pub mechanism: Option<SamplingMechanism>,
}

/// Represents the sampling priority of a trace.
///
/// Positive values indicate the trace should be kept, while zero or negative
/// values indicate rejection. Use the constants in the [`priority`] module
/// for standard priority values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplingPriority {
    value: i8,
}

impl SamplingPriority {
    pub const fn from_i8(value: i8) -> Self {
        Self { value }
    }

    pub fn into_i8(self) -> i8 {
        self.value
    }

    /// Returns whether this sampling priority indicates the trace should be kept.
    ///
    /// # Returns
    ///
    /// `true` if the priority value is positive (indicating the trace should be kept),
    /// `false` otherwise (indicating the trace should be dropped).
    ///
    /// # Examples
    ///
    /// ```
    /// use libdd_sampling::priority;
    ///
    /// assert!(priority::AUTO_KEEP.is_keep());
    /// assert!(priority::USER_KEEP.is_keep());
    /// assert!(!priority::AUTO_REJECT.is_keep());
    /// assert!(!priority::USER_REJECT.is_keep());
    /// ```
    #[inline(always)]
    pub fn is_keep(&self) -> bool {
        self.value > 0
    }
}

/// Sampling priority constants.
///
/// These values indicate whether a trace should be kept or rejected,
/// and whether the decision was made automatically or by the user.
pub mod priority {
    use super::SamplingPriority;

    /// User explicitly rejected this trace (priority -1).
    pub const USER_REJECT: SamplingPriority = SamplingPriority::from_i8(-1);
    /// User explicitly requested to keep this trace (priority 2).
    pub const USER_KEEP: SamplingPriority = SamplingPriority::from_i8(2);
    /// Automatically rejected by the sampler (priority 0).
    pub const AUTO_REJECT: SamplingPriority = SamplingPriority::from_i8(0);
    /// Automatically kept by the sampler (priority 1).
    pub const AUTO_KEEP: SamplingPriority = SamplingPriority::from_i8(1);
}

impl fmt::Display for SamplingPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl FromStr for SamplingPriority {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<i8>() {
            Ok(value) => Ok(SamplingPriority::from_i8(value)),
            Err(_) => Err(()),
        }
    }
}

/// Represents the mechanism that made a sampling decision.
///
/// The sampling mechanism identifies which component or rule determined
/// whether a trace should be sampled (kept or rejected).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SamplingMechanism {
    value: u8,
}

impl SamplingMechanism {
    pub const fn from_u8(value: u8) -> Self {
        Self { value }
    }

    pub fn into_u8(self) -> u8 {
        self.value
    }

    pub fn to_priority(self, is_keep: bool) -> SamplingPriority {
        const AUTO_PAIR: PriorityPair = PriorityPair {
            keep: priority::AUTO_KEEP,
            reject: priority::AUTO_REJECT,
        };
        const USER_PAIR: PriorityPair = PriorityPair {
            keep: priority::USER_KEEP,
            reject: priority::USER_REJECT,
        };
        let pair = match self {
            mechanism::AGENT_RATE_BY_SERVICE | mechanism::DEFAULT => AUTO_PAIR,
            mechanism::MANUAL
            | mechanism::LOCAL_USER_TRACE_SAMPLING_RULE
            | mechanism::REMOTE_USER_TRACE_SAMPLING_RULE
            | mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE
            | mechanism::SPAN_SAMPLING_RULE
            | mechanism::DATA_JOBS_MONITORING => USER_PAIR,
            mechanism::APPSEC => AUTO_PAIR,

            _ => AUTO_PAIR,
        };
        if is_keep {
            pair.keep
        } else {
            pair.reject
        }
    }

    /// Returns the string representation of the sampling mechanism.
    pub fn to_cow(self) -> Cow<'static, str> {
        match self {
            mechanism::DEFAULT => Cow::Borrowed("-0"),
            mechanism::AGENT_RATE_BY_SERVICE => Cow::Borrowed("-1"),
            mechanism::REMOTE_RATE => Cow::Borrowed("-2"),
            mechanism::LOCAL_USER_TRACE_SAMPLING_RULE => Cow::Borrowed("-3"),
            mechanism::MANUAL => Cow::Borrowed("-4"),
            mechanism::APPSEC => Cow::Borrowed("-5"),
            mechanism::REMOTE_RATE_USER => Cow::Borrowed("-6"),
            mechanism::REMOTE_RATE_DATADOG => Cow::Borrowed("-7"),
            mechanism::SPAN_SAMPLING_RULE => Cow::Borrowed("-8"),
            mechanism::OTLP_INGEST_PROBABILISTIC_SAMPLING => Cow::Borrowed("-9"),
            mechanism::DATA_JOBS_MONITORING => Cow::Borrowed("-10"),
            mechanism::REMOTE_USER_TRACE_SAMPLING_RULE => Cow::Borrowed("-11"),
            mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE => Cow::Borrowed("-12"),
            _ => Cow::Owned(self.to_string()),
        }
    }
}

/// Sampling mechanism constants.
///
/// These constants identify which component or rule made a sampling decision.
pub mod mechanism {
    use super::SamplingMechanism;

    /// Default sampling mechanism.
    pub const DEFAULT: SamplingMechanism = SamplingMechanism::from_u8(0);
    /// Agent-based rate sampling by service.
    pub const AGENT_RATE_BY_SERVICE: SamplingMechanism = SamplingMechanism::from_u8(1);
    /// Remote configuration rate sampling.
    pub const REMOTE_RATE: SamplingMechanism = SamplingMechanism::from_u8(2);
    /// Local user-defined trace sampling rule.
    pub const LOCAL_USER_TRACE_SAMPLING_RULE: SamplingMechanism = SamplingMechanism::from_u8(3);
    /// Manual sampling decision via API.
    pub const MANUAL: SamplingMechanism = SamplingMechanism::from_u8(4);
    /// Application Security (AppSec) sampling.
    pub const APPSEC: SamplingMechanism = SamplingMechanism::from_u8(5);
    /// Remote user rate sampling.
    pub const REMOTE_RATE_USER: SamplingMechanism = SamplingMechanism::from_u8(6);
    /// Remote Datadog rate sampling.
    pub const REMOTE_RATE_DATADOG: SamplingMechanism = SamplingMechanism::from_u8(7);
    /// Span-level sampling rule.
    pub const SPAN_SAMPLING_RULE: SamplingMechanism = SamplingMechanism::from_u8(8);
    /// OTLP ingest probabilistic sampling.
    pub const OTLP_INGEST_PROBABILISTIC_SAMPLING: SamplingMechanism = SamplingMechanism::from_u8(9);
    /// Data Jobs Monitoring sampling.
    pub const DATA_JOBS_MONITORING: SamplingMechanism = SamplingMechanism::from_u8(10);
    /// Remote user-defined trace sampling rule.
    pub const REMOTE_USER_TRACE_SAMPLING_RULE: SamplingMechanism = SamplingMechanism::from_u8(11);
    /// Remote dynamic trace sampling rule.
    pub const REMOTE_DYNAMIC_TRACE_SAMPLING_RULE: SamplingMechanism =
        SamplingMechanism::from_u8(12);
}

impl fmt::Display for SamplingMechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "-{}", self.into_u8())
    }
}

impl FromStr for SamplingMechanism {
    type Err = ();

    /// Gets the sampling mechanism from it's string representation.
    fn from_str(s: &str) -> Result<Self, ()> {
        let val: i16 = s.parse().map_err(drop)?;
        if val > 0 {
            return Err(());
        }
        let val = -val;
        if val > u8::MAX as i16 {
            return Err(());
        }
        Ok(SamplingMechanism::from_u8(val as u8))
    }
}

struct PriorityPair {
    keep: SamplingPriority,
    reject: SamplingPriority,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SamplingPriority ---

    #[test]
    fn test_priority_into_i8() {
        assert_eq!(priority::AUTO_KEEP.into_i8(), 1);
        assert_eq!(priority::AUTO_REJECT.into_i8(), 0);
        assert_eq!(priority::USER_KEEP.into_i8(), 2);
        assert_eq!(priority::USER_REJECT.into_i8(), -1);
    }

    #[test]
    fn test_priority_is_keep() {
        assert!(priority::AUTO_KEEP.is_keep());
        assert!(priority::USER_KEEP.is_keep());
        assert!(!priority::AUTO_REJECT.is_keep());
        assert!(!priority::USER_REJECT.is_keep());
    }

    #[test]
    fn test_priority_display() {
        assert_eq!(priority::AUTO_KEEP.to_string(), "1");
        assert_eq!(priority::AUTO_REJECT.to_string(), "0");
        assert_eq!(priority::USER_KEEP.to_string(), "2");
        assert_eq!(priority::USER_REJECT.to_string(), "-1");
    }

    #[test]
    fn test_priority_from_str() {
        assert_eq!(
            "1".parse::<SamplingPriority>().unwrap(),
            priority::AUTO_KEEP
        );
        assert_eq!(
            "0".parse::<SamplingPriority>().unwrap(),
            priority::AUTO_REJECT
        );
        assert_eq!(
            "2".parse::<SamplingPriority>().unwrap(),
            priority::USER_KEEP
        );
        assert_eq!(
            "-1".parse::<SamplingPriority>().unwrap(),
            priority::USER_REJECT
        );
        assert!("not_a_number".parse::<SamplingPriority>().is_err());
        assert!("999".parse::<SamplingPriority>().is_err()); // overflows i8
    }

    // --- SamplingMechanism ---

    #[test]
    fn test_mechanism_into_u8() {
        assert_eq!(mechanism::DEFAULT.into_u8(), 0);
        assert_eq!(mechanism::AGENT_RATE_BY_SERVICE.into_u8(), 1);
        assert_eq!(mechanism::LOCAL_USER_TRACE_SAMPLING_RULE.into_u8(), 3);
        assert_eq!(mechanism::REMOTE_USER_TRACE_SAMPLING_RULE.into_u8(), 11);
        assert_eq!(mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE.into_u8(), 12);
    }

    #[test]
    fn test_mechanism_to_priority_auto_pair() {
        // DEFAULT and AGENT_RATE_BY_SERVICE use AUTO priority
        assert_eq!(mechanism::DEFAULT.to_priority(true), priority::AUTO_KEEP);
        assert_eq!(mechanism::DEFAULT.to_priority(false), priority::AUTO_REJECT);
        assert_eq!(
            mechanism::AGENT_RATE_BY_SERVICE.to_priority(true),
            priority::AUTO_KEEP
        );
        assert_eq!(
            mechanism::AGENT_RATE_BY_SERVICE.to_priority(false),
            priority::AUTO_REJECT
        );
        assert_eq!(mechanism::APPSEC.to_priority(true), priority::AUTO_KEEP);
        assert_eq!(mechanism::APPSEC.to_priority(false), priority::AUTO_REJECT);
    }

    #[test]
    fn test_mechanism_to_priority_user_pair() {
        // Rule-based mechanisms use USER priority
        assert_eq!(
            mechanism::LOCAL_USER_TRACE_SAMPLING_RULE.to_priority(true),
            priority::USER_KEEP
        );
        assert_eq!(
            mechanism::LOCAL_USER_TRACE_SAMPLING_RULE.to_priority(false),
            priority::USER_REJECT
        );
        assert_eq!(
            mechanism::REMOTE_USER_TRACE_SAMPLING_RULE.to_priority(true),
            priority::USER_KEEP
        );
        assert_eq!(
            mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE.to_priority(true),
            priority::USER_KEEP
        );
        assert_eq!(mechanism::MANUAL.to_priority(true), priority::USER_KEEP);
        assert_eq!(
            mechanism::SPAN_SAMPLING_RULE.to_priority(false),
            priority::USER_REJECT
        );
        assert_eq!(
            mechanism::DATA_JOBS_MONITORING.to_priority(true),
            priority::USER_KEEP
        );
    }

    #[test]
    fn test_mechanism_to_priority_unknown_falls_back_to_auto() {
        let unknown = SamplingMechanism::from_u8(99);
        assert_eq!(unknown.to_priority(true), priority::AUTO_KEEP);
        assert_eq!(unknown.to_priority(false), priority::AUTO_REJECT);
    }

    #[test]
    fn test_mechanism_to_cow() {
        assert_eq!(mechanism::DEFAULT.to_cow(), "-0");
        assert_eq!(mechanism::AGENT_RATE_BY_SERVICE.to_cow(), "-1");
        assert_eq!(mechanism::REMOTE_RATE.to_cow(), "-2");
        assert_eq!(mechanism::LOCAL_USER_TRACE_SAMPLING_RULE.to_cow(), "-3");
        assert_eq!(mechanism::MANUAL.to_cow(), "-4");
        assert_eq!(mechanism::APPSEC.to_cow(), "-5");
        assert_eq!(mechanism::REMOTE_RATE_USER.to_cow(), "-6");
        assert_eq!(mechanism::REMOTE_RATE_DATADOG.to_cow(), "-7");
        assert_eq!(mechanism::SPAN_SAMPLING_RULE.to_cow(), "-8");
        assert_eq!(mechanism::OTLP_INGEST_PROBABILISTIC_SAMPLING.to_cow(), "-9");
        assert_eq!(mechanism::DATA_JOBS_MONITORING.to_cow(), "-10");
        assert_eq!(mechanism::REMOTE_USER_TRACE_SAMPLING_RULE.to_cow(), "-11");
        assert_eq!(
            mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE.to_cow(),
            "-12"
        );
        // Unknown mechanism falls back to Display
        assert_eq!(SamplingMechanism::from_u8(99).to_cow(), "-99");
    }

    #[test]
    fn test_mechanism_display() {
        assert_eq!(mechanism::DEFAULT.to_string(), "-0");
        assert_eq!(mechanism::LOCAL_USER_TRACE_SAMPLING_RULE.to_string(), "-3");
        assert_eq!(
            mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE.to_string(),
            "-12"
        );
    }

    #[test]
    fn test_mechanism_from_str() {
        assert_eq!(
            "-0".parse::<SamplingMechanism>().unwrap(),
            mechanism::DEFAULT
        );
        assert_eq!(
            "-1".parse::<SamplingMechanism>().unwrap(),
            mechanism::AGENT_RATE_BY_SERVICE
        );
        assert_eq!(
            "-3".parse::<SamplingMechanism>().unwrap(),
            mechanism::LOCAL_USER_TRACE_SAMPLING_RULE
        );
        assert_eq!(
            "-12".parse::<SamplingMechanism>().unwrap(),
            mechanism::REMOTE_DYNAMIC_TRACE_SAMPLING_RULE
        );
    }

    #[test]
    fn test_mechanism_from_str_errors() {
        assert!("not_a_number".parse::<SamplingMechanism>().is_err());
        assert!("1".parse::<SamplingMechanism>().is_err()); // positive not allowed
        assert!("-99999".parse::<SamplingMechanism>().is_err()); // overflows u8
    }
}
