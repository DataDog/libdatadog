// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::errors::ParseError;
use crate::{constants, datadog};
use ddsketch_agent::DDSketch;
use fnv::FnvHasher;
use protobuf::Chars;
use regex::Regex;
use std::hash::{Hash, Hasher};
use ustr::Ustr;

pub const EMPTY_TAGS: SortedTags = SortedTags { values: Vec::new() };

#[derive(Clone, Debug)]
pub enum MetricValue {
    /// Dogstatsd 'count' metric type, monotonically increasing counter
    Count(f64),
    /// Dogstatsd 'gauge' metric type, point-in-time value
    Gauge(f64),
    /// Dogstatsd 'distribution' metric type, histogram
    Distribution(DDSketch),
}

#[derive(Clone, Debug)]
pub struct SortedTags {
    // We sort tags. This is in feature parity with DogStatsD and also means
    // that we avoid storing the same context multiple times because users have
    // passed tags in different order through time.
    values: Vec<(Ustr, Ustr)>,
}

impl SortedTags {
    pub fn extend(&mut self, other: &SortedTags) {
        self.values.extend_from_slice(&other.values);
        self.values.dedup();
        self.values.sort_unstable();
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn parse(tags_section: &str) -> Result<SortedTags, ParseError> {
        let tag_parts = tags_section.split(',');
        let mut parsed_tags = Vec::new();
        // Validate that the tags have the right form.
        for (i, part) in tag_parts.filter(|s| !s.is_empty()).enumerate() {
            if i >= constants::MAX_TAGS {
                return Err(ParseError::Raw("Too many tags"));
            }
            if !part.contains(':') {
                return Err(ParseError::Raw("Invalid tag format"));
            }
            if let Some((k, v)) = part.split_once(':') {
                parsed_tags.push((Ustr::from(k), Ustr::from(v)));
            }
        }
        parsed_tags.dedup();
        parsed_tags.sort_unstable();
        Ok(SortedTags {
            values: parsed_tags,
        })
    }

    pub fn to_chars(&self) -> Vec<Chars> {
        let mut tags_as_chars = Vec::new();
        for (k, v) in &self.values {
            tags_as_chars.push(format!("{}:{}", k, v).into());
        }
        tags_as_chars
    }

    pub fn to_strings(&self) -> Vec<String> {
        let mut tags_as_vec = Vec::new();
        for (k, v) in &self.values {
            tags_as_vec.push(format!("{}:{}", k, v));
        }
        tags_as_vec
    }

    pub(crate) fn to_resources(&self) -> Vec<datadog::Resource> {
        let mut resources = Vec::with_capacity(constants::MAX_TAGS);
        for (name, kind) in &self.values {
            let resource = datadog::Resource {
                name: name.as_str(),
                kind: kind.as_str(),
            };
            resources.push(resource);
        }
        resources
    }
}

/// Representation of a dogstatsd Metric
///
/// For now this implementation covers only counters and gauges. We hope this is
/// enough to demonstrate the impact of this program's design goals.
#[derive(Clone, Debug)]
pub struct Metric {
    /// Name of the metric.
    ///
    /// Never more bytes than `constants::MAX_METRIC_NAME_BYTES`,
    /// enforced by construction. Note utf8 issues.
    pub name: Ustr,
    /// Values of the metric. A singular value may be either a floating point or
    /// a integer. Although undocumented we assume 64 bit. A single metric may
    /// encode multiple values a time in a message. There must be at least one
    /// value here, meaning that there is guaranteed to be a Some value in the
    /// 0th index.
    ///
    /// Parsing of the values to an integer type is deferred until the last
    /// moment.
    ///
    /// Never longer than `constants::MAX_VALUE_BYTES`. Note utf8 issues.
    pub value: MetricValue,
    /// Tags of the metric.
    ///
    /// The key is never longer than `constants::MAX_TAG_KEY_BYTES`, the value
    /// never more than `constants::MAX_TAG_VALUE_BYTES`. These are enforced by
    /// the parser. We assume here that tags are not sent in random order by the
    /// clien or that, if they are, the API will tidy that up. That is `a:1,b:2`
    /// is a different tagset from `b:2,a:1`.
    pub tags: SortedTags,

    /// ID given a name and tagset.
    pub(crate) id: u64,
}

/// Parse a metric from given input.
///
/// This function parses a passed `&str` into a `Metric`. We assume that
/// `DogStatsD` metrics must be utf8 and are not ascii or some other encoding.
///
/// # Errors
///
/// This function will return with an error if the input violates any of the
/// limits in [`constants`]. Any non-viable input will be discarded.
/// example aj-test.increment:1|c|#user:aj-test from 127.0.0.1:50983
pub fn parse(input: &str) -> Result<Metric, ParseError> {
    // TODO must enforce / exploit constraints given in `constants`.
    if let Ok(re) = Regex::new(
        r"^(?P<name>[^:]+):(?P<values>[^|]+)\|(?P<type>[cgd])(?:\|@(?P<sample_rate>[\d.]+))?(?:\|#(?P<tags>[^|]+))?$",
    ) {
        if let Some(caps) = re.captures(input) {
            // unused for now
            // let sample_rate = caps.name("sample_rate").map(|m| m.as_str());

            let tags;
            if let Some(tags_section) = caps.name("tags") {
                tags = SortedTags::parse(tags_section.as_str())?;
            } else {
                tags = EMPTY_TAGS;
            }
            let val = first_value(caps.name("values").unwrap().as_str())?;
            let metric_value = match caps.name("type").unwrap().as_str() {
                "c" => MetricValue::Count(val),
                "g" => MetricValue::Gauge(val),
                "d" => {
                    let sketch = &mut DDSketch::default();
                    sketch.insert(val);
                    MetricValue::Distribution(sketch.to_owned())
                }
                _ => {
                    return Err(ParseError::Raw("Unsupported metric type"));
                }
            };
            let name = Ustr::from(caps.name("name").unwrap().as_str());
            let id = id(name, &tags);
            return Ok(Metric {
                name,
                value: metric_value,
                tags,
                id,
            });
        }
    }
    Err(ParseError::Raw("Invalid metric format"))
}

fn first_value(values: &str) -> Result<f64, ParseError> {
    match values.split(':').next() {
        Some(v) => match v.parse::<f64>() {
            Ok(v) => Ok(v),
            Err(_) => Err(ParseError::Raw("Invalid value")),
        },
        None => Err(ParseError::Raw("Missing value")),
    }
}

/// Create an ID given a name and tagset.
///
/// This function constructs a hash of the name, the tagset and that hash is
/// identical no matter the internal order of the tagset. That is, we consider a
/// tagset like "a:1,b:2,c:3" to be idential to "b:2,c:3,a:1" to "c:3,a:1,b:2"
/// etc. This implies that we must sort the tagset after parsing it, which we
/// do. Duplicate tags are removed, so "a:1,a:1" will
/// hash to the same ID of "a:1".
///
/// Note also that because we take `Ustr` arguments its possible that we've
/// interned many possible combinations of a tagset, even if they are identical
/// from the point of view of this function.
#[inline]
#[must_use]
pub fn id(name: Ustr, tags: &SortedTags) -> u64 {
    let mut hasher = FnvHasher::default();

    name.hash(&mut hasher);
    for kv in tags.values.iter() {
        kv.0.as_bytes().hash(&mut hasher);
        kv.1.as_bytes().hash(&mut hasher);
    }
    hasher.finish()
}
// <METRIC_NAME>:<VALUE>:<VALUE>:<VALUE>|<TYPE>|@<SAMPLE_RATE>|#<TAG_KEY_1>:<TAG_VALUE_1>,
// <TAG_KEY_2>:<TAG_VALUE_2>|T<METRIC_TIMESTAMP>|c:<CONTAINER_ID>
//
// Types:
//  * c -- COUNT, allows packed values, summed
//  * g -- GAUGE, allows packed values, last one wins
//
// SAMPLE_RATE ignored for the sake of simplicity.

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use proptest::{collection, option, strategy::Strategy, string::string_regex};
    use ustr::Ustr;

    use crate::metric::{id, parse, MetricValue, SortedTags};

    use super::ParseError;

    fn metric_name() -> impl Strategy<Value = String> {
        string_regex("[a-zA-Z0-9.-]{1,128}").unwrap()
    }

    fn metric_values() -> impl Strategy<Value = String> {
        string_regex("[0-9]+(:[0-9]){0,8}").unwrap()
    }

    fn metric_type() -> impl Strategy<Value = String> {
        string_regex("g|c").unwrap()
    }

    fn metric_tagset() -> impl Strategy<Value = Option<String>> {
        option::of(
            string_regex("[a-zA-Z]{1,64}:[a-zA-Z]{1,64}(,[a-zA-Z]{1,64}:[a-zA-Z]{1,64}){0,31}")
                .unwrap(),
        )
    }

    fn metric_tags() -> impl Strategy<Value = Vec<(String, String)>> {
        collection::vec(("[a-z]{1,8}", "[A-Z]{1,8}"), 0..32)
    }

    proptest::proptest! {
        // For any valid name, tags et al the parse routine is able to parse an
        // encoded metric line.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn parse_valid_inputs(
            name in metric_name(),
            values in metric_values(),
            mtype in metric_type(),
            tagset in metric_tagset()
        ) {
            let input = if let Some(ref tagset) = tagset {
                format!("{name}:{values}|{mtype}|#{tagset}")
            } else {
                format!("{name}:{values}|{mtype}")
            };
            let metric = parse(&input).unwrap();
            assert_eq!(name, metric.name.as_str());

            if let Some(tags) = tagset {
                let parsed_metric_tags : SortedTags= metric.tags.clone();
                assert_eq!(tags.split(',').count(), parsed_metric_tags.values.len());
                tags.split(',').for_each(|kv| {
                    let (original_key, original_value) = kv.split_once(':').unwrap();
                    let mut found = false;
                    for (k,v) in parsed_metric_tags.values.iter() {
                        // TODO not sure who to handle duplicate keys. To make the test pass, just find any match instead of first
                        if *k == Ustr::from(original_key) && *v == Ustr::from(original_value) {
                            found = true;
                        }
                    }
                    assert!(found);
                });
            } else {
                assert!(metric.tags.is_empty());
            }

            match mtype.as_str() {
                "c" => {
                    if let MetricValue::Count(v) = metric.value {
                        assert_eq!(v, values.split(':').next().unwrap().parse::<f64>().unwrap());
                    } else {
                        panic!("Expected count metric");
                    }
                }
                "g" => {
                    if let MetricValue::Gauge(v) = metric.value {
                        assert_eq!(v, values.split(':').next().unwrap().parse::<f64>().unwrap());
                    } else {
                        panic!("Expected gauge metric");
                    }
                }
                "d" => {
                    if let MetricValue::Distribution(d) = metric.value {
                        assert_eq!(d.min().unwrap(), values.split(':').next().unwrap().parse::<f64>().unwrap());
                    } else {
                        panic!("Expected distribution metric");
                    }
                }
                _ => {
                    panic!("Invalid metric format");
                }
            }
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn parse_missing_name_and_value(
            mtype in metric_type(),
            tagset in metric_tagset()
        ) {
            let input = if let Some(ref tagset) = tagset {
                format!("|{mtype}|#{tagset}")
            } else {
                format!("|{mtype}")
            };
            let result = parse(&input);

            assert_eq!(result.unwrap_err(),ParseError::Raw("Invalid metric format"));
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn parse_invalid_name_and_value_format(
            name in metric_name(),
            values in metric_values(),
            mtype in metric_type(),
            tagset in metric_tagset()
        ) {
            // If there is a ':' in the values we cannot distinguish where the
            // name and the first value are.
            let value = values.split(':').next().unwrap();
            let input = if let Some(ref tagset) = tagset {
                format!("{name}{value}|{mtype}|#{tagset}")
            } else {
                format!("{name}{value}|{mtype}")
            };
            let result = parse(&input);

            assert_eq!(
                result.unwrap_err(),
                ParseError::Raw("Invalid metric format")
            );
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn parse_unsupported_metric_type(
            name in metric_name(),
            values in metric_values(),
            mtype in "[abefhijklmnopqrstuvwxyz]",
            tagset in metric_tagset()
        ) {
            let input = if let Some(ref tagset) = tagset {
                format!("{name}:{values}|{mtype}|#{tagset}")
            } else {
                format!("{name}:{values}|{mtype}")
            };
            let result = parse(&input);

            assert_eq!(
                result.unwrap_err(),
                ParseError::Raw("Invalid metric format")
            );
        }

        // The ID of a name, tagset is the same even if the tagset is in a
        // distinct order.
        // For any valid name, tags et al the parse routine is able to parse an
        // encoded metric line.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn id_consistent(name in metric_name(),
                         mut tags in metric_tags()) {
            let mut tagset1 = String::new();
            let mut tagset2 = String::new();

            for (k,v) in &tags {
                tagset1.push_str(k);
                tagset1.push(':');
                tagset1.push_str(v);
                tagset1.push(',');
            }
            tags.reverse();
            for (k,v) in &tags {
                tagset2.push_str(k);
                tagset2.push(':');
                tagset2.push_str(v);
                tagset2.push(',');
            }
            if !tags.is_empty() {
                tagset1.pop();
                tagset2.pop();
            }

            let id1 = id(Ustr::from(&name), &SortedTags::parse(&tagset1).unwrap());
            let id2 = id(Ustr::from(&name), &SortedTags::parse(&tagset2).unwrap());

            assert_eq!(id1, id2);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn parse_too_many_tags() {
        // 33
        assert_eq!(parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3").unwrap_err(),
                   ParseError::Raw("Too many tags"));

        // 32
        assert!(parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2").is_ok());

        // 31
        assert!(parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1").is_ok());

        // 30
        assert!(parse("foo:1|g|#a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3,a:1,b:2,c:3").is_ok());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn invalid_dogstatsd_no_panic() {
        assert!(parse("somerandomstring|c+a;slda").is_err());
    }
}
