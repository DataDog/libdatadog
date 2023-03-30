// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::tag::{parse_tags, Tag};

#[derive(PartialEq, Debug)]
pub struct Metric {
    name: String,
    values: Vec<f64>,
    metric_type: MetricType,
    sample_rate: f64,
    tags: Vec<Tag>,
}

#[derive(PartialEq, Debug)]
pub enum MetricType {
    Counter,
    Gauge,
    Timer,
    Set,
    Distribution,
}

impl Metric {
    pub fn from_string(s: &str) -> Option<Self> {
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

        let values: Vec<f64> = values_str
            .split(',')
            .filter_map(|value| value.parse().ok())
            .collect();

        let metric_type = match type_str {
            "c" => MetricType::Counter,
            "g" => MetricType::Gauge,
            "ms" => MetricType::Timer,
            "s" => MetricType::Set,
            "d" => MetricType::Distribution,
            _ => return None,
        };

        if metric_type != MetricType::Distribution {
            return None;
        }

        let mut sample_rate = 1.0;
        let mut tags = vec![];

        // The first 2 tokens are metric name and values, which we have parsed above
        // The next 2 tokens are optional, and are a combination of sampling_rate and tags
        for token in &tokens[2..] {
            let identifier = token.chars().next()?;
            match identifier {
                '@' => sample_rate = token[1..].parse::<f64>().unwrap_or(1.0),
                '#' => {
                    let (parsed_tags, _) = parse_tags(&token[1..]);
                    tags = parsed_tags;
                }
                _ => {}
            }
        }

        Some(Self {
            name: metric_name.to_string(),
            values,
            metric_type,
            sample_rate,
            tags,
        })
    }
}

mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_parse_metric_distribution() {
        let input = "my.distribution:10.2,13.1,14.5,15.0|d|#tag1,tag2:value2";

        let expected = Metric {
            name: "my.distribution".to_string(),
            values: vec![10.2, 13.1, 14.5, 15.0],
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![
                Tag::from_value("tag1").unwrap(),
                Tag::new("tag2", "value2").unwrap(),
            ],
        };

        assert_eq!(Metric::from_string(input), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_one_value() {
        let input = "my.distribution:10|d|#tag1,tag2:value2";

        let expected = Metric {
            name: "my.distribution".to_string(),
            values: vec![10.0],
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![
                Tag::from_value("tag1").unwrap(),
                Tag::new("tag2", "value2").unwrap(),
            ],
        };

        assert_eq!(Metric::from_string(input), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_no_tag() {
        let input = "my.distribution:10|d";

        let expected = Metric {
            name: "my.distribution".to_string(),
            values: vec![10.0],
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![],
        };

        assert_eq!(Metric::from_string(input), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_empty() {
        let input = "";
        assert_eq!(Metric::from_string(input), None);
    }

    #[test]
    fn test_parse_metric_distribution_with_sample_rate() {
        let input = "my.distribution:10|d|@1.0";

        let expected = Metric {
            name: "my.distribution".to_string(),
            values: vec![10.0],
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![],
        };

        assert_eq!(Metric::from_string(input), Some(expected));
    }

    #[test]
    fn test_parse_metric_count() {
        let input = "my.distribution:10|c|@1.0";

        assert_eq!(Metric::from_string(input), None);
    }

    #[test]
    fn test_parse_metric_distribution_with_malformed_sample_rate_and_tags() {
        let input = "my.distribution:10|d|@a|#";

        let expected = Metric {
            name: "my.distribution".to_string(),
            values: vec![10.0],
            metric_type: MetricType::Distribution,
            sample_rate: 1.0,
            tags: vec![],
        };

        assert_eq!(Metric::from_string(input), Some(expected));
    }

    #[test]
    fn test_parse_metric_distribution_malformed_inputs() {
        let input = ":|d|@1.0";
        assert_eq!(Metric::from_string(input), None);

        let input2 = "";
        assert_eq!(Metric::from_string(input2), None);

        let input3 = "||||";
        assert_eq!(Metric::from_string(input3), None);

        let input4 = ":@:@:@:@";
        assert_eq!(Metric::from_string(input4), None);
    }
}
