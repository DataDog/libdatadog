// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::tag::Tag;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

/// The `DogStatsDActionOwned` enum gathers the metric types that can be sent to the DogStatsD
/// server. This type takes ownership of the relevant data to support the sidecar better.
/// For documentation on the dogstatsd metric types: https://docs.datadoghq.com/metrics/types/?tab=count#metric-types
///
/// Originally I attempted to combine this type with `DogStatsDAction` but this GREATLY complicates
/// the types to the point of insanity. I was unable to come up with a satisfactory approach that
/// allows both the data-pipeline and sidecar crates to use the same type. If a future rustacean
/// wants to take a stab and open a PR please do so!
#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDActionOwned {
    #[allow(missing_docs)]
    Count(String, i64, Vec<Tag>),
    #[allow(missing_docs)]
    Distribution(String, f64, Vec<Tag>),
    #[allow(missing_docs)]
    Gauge(String, f64, Vec<Tag>),
    #[allow(missing_docs)]
    Histogram(String, f64, Vec<Tag>),
    /// Cadence only support i64 type as value
    /// but Golang implementation uses string (https://github.com/DataDog/datadog-go/blob/331d24832f7eac97b091efd696278fe2c4192b29/statsd/statsd.go#L230)
    /// and PHP implementation uses float or string (https://github.com/DataDog/php-datadogstatsd/blob/0efdd1c38f6d3dd407efbb899ad1fd2e5cd18085/src/DogStatsd.php#L251)
    Set(String, i64, Vec<Tag>),
}

/// The `DogStatsDAction` enum gathers the metric types that can be sent to the DogStatsD server.
#[derive(Debug, Serialize, Deserialize)]
pub enum DogStatsDAction<'a, T: AsRef<str>, V: IntoIterator<Item = &'a Tag>> {
    // TODO: instead of AsRef<str> we can accept a marker Trait that users of this crate implement
    #[allow(missing_docs)]
    Count(T, i64, V),
    #[allow(missing_docs)]
    Distribution(T, f64, V),
    #[allow(missing_docs)]
    Gauge(T, f64, V),
    #[allow(missing_docs)]
    Histogram(T, f64, V),
    /// Cadence only support i64 type as value
    /// but Golang implementation uses string (https://github.com/DataDog/datadog-go/blob/331d24832f7eac97b091efd696278fe2c4192b29/statsd/statsd.go#L230)
    /// and PHP implementation uses float or string (https://github.com/DataDog/php-datadogstatsd/blob/0efdd1c38f6d3dd407efbb899ad1fd2e5cd18085/src/DogStatsd.php#L251)
    Set(T, i64, V),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_owned_sync() {
        // This test ensures that if a new variant is added to either `DogStatsDActionOwned` or
        // `DogStatsDAction` this test will NOT COMPILE to act as a reminder that BOTH locations
        // must be updated.
        let owned_act = DogStatsDActionOwned::Count("test".to_string(), 1, vec![]);
        match owned_act {
            DogStatsDActionOwned::Count(_, _, _) => {}
            DogStatsDActionOwned::Distribution(_, _, _) => {}
            DogStatsDActionOwned::Gauge(_, _, _) => {}
            DogStatsDActionOwned::Histogram(_, _, _) => {}
            DogStatsDActionOwned::Set(_, _, _) => {}
        }

        let act = DogStatsDAction::Count("test".to_string(), 1, vec![]);
        match act {
            DogStatsDAction::Count(_, _, _) => {}
            DogStatsDAction::Distribution(_, _, _) => {}
            DogStatsDAction::Gauge(_, _, _) => {}
            DogStatsDAction::Histogram(_, _, _) => {}
            DogStatsDAction::Set(_, _, _) => {}
        }
        // TODO: when std::mem::variant_count is in stable we can do this instead
        // assert_eq!(
        //     std::mem::variant_count::<DogStatsDActionOwned>(),
        //     std::mem::variant_count::<DogStatsDAction<String, Vec<&Tag>>>(),
        //     "DogStatsDActionOwned and DogStatsDAction should have the same number of variants,
        // did you forget to update one?", );
    }
}
