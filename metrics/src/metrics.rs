// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::tag::{parse_tags, Tag};
use serde::Serialize;

#[derive(PartialEq, Debug, Serialize, Clone)]
pub struct Points {
    pub timestamp: u64,
    pub values: Vec<f64>,
}

impl Points {
    pub fn new(values: Vec<f64>, timestamp: u64) -> Self {
        Self { timestamp, values }
    }
}

#[derive(PartialEq, Debug, Serialize, Clone)]
pub enum MetricType {
    Distribution,
}

#[derive(PartialEq, Debug, Serialize, Clone)]
pub struct Metric {
    pub metric: String,
    pub points: Points,
    pub tags: Vec<Tag>,
    #[serde(skip)]
    metric_type: MetricType,
    #[serde(skip)]
    sample_rate: f64,
}

impl Metric {
    // from_string takes in a single line from a dogstatsd udp packet
    // and parses it into a Metric struct.
    pub fn from_string(s: &str, timestamp: u64) -> Option<Self> {
        let (metric_name, parts) = s.split_once(':')?;
        if metric_name.is_empty() {
            return None;
        }

        let tokens: Vec<&str> = parts.split('|').collect();
        if tokens.len() < 2 {
            return None;
        }

        let values_str = tokens[0];
        let type_str = tokens[1];

        let points = Points::new(
            values_str
                .split(',')
                .filter_map(|value| value.parse().ok())
                .collect(),
            timestamp,
        );

        let metric_type = match type_str {
            // Only support Distribution metrics for now
            "d" => MetricType::Distribution,
            _ => return None,
        };

        let mut sample_rate = 1.0;
        let mut tags = vec![];

        // The first 2 tokens are metric name and values, which we have parsed above
        // The next 2 tokens are optional, and are a combination of sampling_rate and tags
        for token in &tokens[2..] {
            let identifier = token.chars().next()?;
            match identifier {
                '@' => sample_rate = token[1..].parse::<f64>().unwrap_or(1.0),
                '#' => tags = parse_tags(&token[1..]).0,
                _ => {}
            }
        }

        Some(Self {
            metric: metric_name.to_string(),
            points,
            metric_type,
            sample_rate,
            tags,
        })
    }

    pub fn new(metric: String, points: Points, tags: Vec<Tag>) -> Self {
        Self {
            metric,
            points,
            tags,
            sample_rate: 1.0,
            metric_type: MetricType::Distribution,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_metric_distribution() {
        let input = "my.distribution:10.2,13.1,14.5,15.0|d|#tag1,tag2:value2";

        let expected = Metric {
            metric: "my.distribution".to_string(),
            points: Points::new(vec![10.2, 13.1, 14.5, 15.0], 0),
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![
                Tag::from_value("tag1").unwrap(),
                Tag::new("tag2", "value2").unwrap(),
            ],
        };

        assert_eq!(Metric::from_string(input, 0), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_one_value() {
        let input = "my.distribution:10|d|#tag1,tag2:value2";

        let expected = Metric {
            metric: "my.distribution".to_string(),
            points: Points::new(vec![10.0], 0),
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![
                Tag::from_value("tag1").unwrap(),
                Tag::new("tag2", "value2").unwrap(),
            ],
        };

        assert_eq!(Metric::from_string(input, 0), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_no_tag() {
        let input = "my.distribution:10|d";

        let expected = Metric {
            metric: "my.distribution".to_string(),
            points: Points::new(vec![10.0], 0),
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![],
        };

        assert_eq!(Metric::from_string(input, 0), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_empty() {
        let input = "";
        assert_eq!(Metric::from_string(input, 0), None);
    }

    #[test]
    fn test_parse_metric_distribution_with_sample_rate() {
        let input = "my.distribution:10|d|@1.0";

        let expected = Metric {
            metric: "my.distribution".to_string(),
            points: Points::new(vec![10.0], 0),
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![],
        };

        assert_eq!(Metric::from_string(input, 0), Some(expected));
    }

    #[test]
    fn test_parse_metric_count() {
        let input = "my.distribution:10|c|@1.0";

        assert_eq!(Metric::from_string(input, 0), None);
    }

    #[test]
    fn test_parse_metric_distribution_with_malformed_sample_rate_and_tags() {
        let input = "my.distribution:10|d|@a|#";

        let expected = Metric {
            metric: "my.distribution".to_string(),
            points: Points::new(vec![10.0], 0),
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![],
        };

        assert_eq!(Metric::from_string(input, 0), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_malformed_inputs() {
        let input = ":|d|@1.0";
        assert_eq!(Metric::from_string(input, 0), None);

        let input2 = "";
        assert_eq!(Metric::from_string(input2, 0), None);

        let input3 = "||||";
        assert_eq!(Metric::from_string(input3, 0), None);

        let input4 = ":@:@:@:@";
        assert_eq!(Metric::from_string(input4, 0), None);
    }
}
