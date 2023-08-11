use crate::expr_defs::{Condition, StringSource, Value};
use crate::parse_json_expr::{parse_condition, parse_segments, parse_value};
use crate::parse_util::get;
use crate::{
    Capture, EvaluateAt, FilterList, InBodyLocation, LiveDebuggingData, LogProbe, MetricKind,
    MetricProbe, Probe, ProbeCondition, ProbeTarget, ProbeType, ProbeValue, ServiceConfiguration,
    SpanDecorationProbe, SpanProbe, SpanProbeDecoration, SpanProbeTarget,
};
use json::JsonValue;

pub type ParseResult<T> = Result<T, ()>;

fn parse_string_vec(array: &JsonValue) -> ParseResult<Vec<String>> {
    let mut vec = vec![];
    if !array.is_array() {
        return Err(());
    }
    for value in array.members() {
        vec.push(value.as_str().ok_or(())?.to_string());
    }
    Ok(vec)
}

fn parse_probe(parsed: &JsonValue) -> ParseResult<Probe> {
    let mut tags = vec![];
    if let Ok(json_tags) = get(parsed, "tags") {
        if !json_tags.is_array() {
            return Err(());
        }
        for tag in json_tags.members() {
            tags.push(tag.as_str().ok_or(())?.into());
        }
    }

    let target = get(parsed, "where")?;
    let lines = if let Ok(lines) = get(target, "lines") {
        parse_string_vec(get(lines, "where")?)?
    } else {
        vec![]
    };

    let target_get = |name: &str| -> ParseResult<Option<String>> {
        get(target, name)
            .ok()
            .and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    Some(v.as_str().map(ToString::to_string).ok_or(()))
                }
            })
            .transpose()
    };
    let probe = match get(parsed, "type")?.as_str().ok_or(())? {
        "METRIC_PROBE" => ProbeType::Metric(MetricProbe {
            kind: match get(parsed, "kind")?.as_str().ok_or(())? {
                "COUNT" => MetricKind::Count,
                "GAUGE" => MetricKind::Gauge,
                "HISTOGRAM" => MetricKind::Histogram,
                "DISTRIBUTION" => MetricKind::Distribution,
                _ => return Err(()),
            },
            name: get(parsed, "metricName")?.as_str().ok_or(())?.to_string(),
            value: ProbeValue(
                get(parsed, "value")
                    .ok()
                    .and_then(|v| {
                        if v.is_null() {
                            None
                        } else {
                            Some(parse_value(v))
                        }
                    })
                    .transpose()?
                    .unwrap_or(Value::String(StringSource::Null)),
            ),
        }),
        "LOG_PROBE" => ProbeType::Log(LogProbe {
            segments: parse_segments(get(parsed, "segment")?)?,
            when: ProbeCondition(
                get(parsed, "when")
                    .map(|v| parse_condition(v))
                    .unwrap_or(Ok(Condition::Always))?,
            ),
            capture: {
                let mut capture = Capture::default();
                if let Ok(v) = get(parsed, "capture") {
                    if !v.is_null() {
                        if let Ok(max_reference_depth) = get(v, "maxReference_depth") {
                            capture.max_reference_depth = max_reference_depth.as_u32().ok_or(())?;
                        }
                        if let Ok(max_collection_size) = get(v, "maxCollectionSize") {
                            capture.max_collection_size = max_collection_size.as_u32().ok_or(())?;
                        }
                        if let Ok(max_length) = get(v, "maxLength") {
                            capture.max_length = max_length.as_u32().ok_or(())?;
                        }
                        if let Ok(max_field_depth) = get(v, "maxFieldDepth") {
                            capture.max_field_depth = max_field_depth.as_u32().ok_or(())?;
                        }
                    }
                }
                capture
            },
            sampling_snapshots_per_second: get(parsed, "sampling")
                .ok()
                .and_then(|v| {
                    if v.is_null() {
                        None
                    } else {
                        Some(v.as_u32().ok_or(()))
                    }
                })
                .transpose()?
                .unwrap_or(5000),
        }),
        "SPAN_PROBE" => ProbeType::Span(SpanProbe {}),
        "SPAN_DECORATION_PROBE" => ProbeType::SpanDecoration(SpanDecorationProbe {
            target: match get(parsed, "").map_or_else(|_| Ok("ACTIVE"), |v| v.as_str().ok_or(()))? {
                "ACTIVE" => SpanProbeTarget::Active,
                "ROOT" => SpanProbeTarget::Root,
                _ => return Err(()),
            },
            decorations: {
                let mut vec = vec![];
                let decorations = get(parsed, "decorations")?;
                if !decorations.is_array() {
                    return Err(());
                }
                for decoration in decorations.members() {
                    let tags = get(decoration, "tags")?;
                    if !tags.is_array() {
                        return Err(());
                    }
                    let mut tagvec = vec![];
                    for tag in tags.members() {
                        let name = get(tag, "name")?.as_str().ok_or(())?.to_string();
                        let value = parse_segments(get(tag, "value")?)?;
                        tagvec.push((name, value));
                    }
                    let condition = get(decoration, "when")
                        .ok()
                        .and_then(|v| {
                            if v.is_null() {
                                None
                            } else {
                                Some(parse_condition(v))
                            }
                        })
                        .transpose()?
                        .unwrap_or(Condition::Always);
                    vec.push(SpanProbeDecoration {
                        condition: ProbeCondition(condition),
                        tags: tagvec,
                    });
                }
                vec
            },
        }),
        _ => return Err(()),
    };

    Ok(Probe {
        id: get(parsed, "id")?.as_str().ok_or(())?.into(),
        version: get(parsed, "version")?.as_u64().unwrap_or(0),
        language: get(parsed, "language")?.as_str().map(ToString::to_string),
        tags,
        target: ProbeTarget {
            type_name: target_get("typeName")?,
            method_name: target_get("methodName")?,
            source_file: target_get("sourcFile")?,
            signature: target_get("signature")?,
            lines,
            in_body_location: match target_get("inBodyLocation")? {
                None => InBodyLocation::None,
                Some(string) => match string.as_str() {
                    "START" => InBodyLocation::Start,
                    "END" => InBodyLocation::End,
                    _ => return Err(()),
                },
            },
        },
        evaluate_at: match get(parsed, "evaluateAt")?.as_str().ok_or(())? {
            "ENTRY" => EvaluateAt::Entry,
            "EXIT" => EvaluateAt::Exit,
            _ => return Err(()),
        },
        probe,
    })
}

fn parse_service_configuration(parsed: &JsonValue) -> ParseResult<ServiceConfiguration> {
    if get(parsed, "type")?.as_str().ok_or(())? != "SERVICE_CONFIGURATION" {
        return Err(());
    }

    fn parse_filter_list(parsed: &JsonValue, key: &str) -> ParseResult<FilterList> {
        let f = get(parsed, key)?;
        Ok(FilterList {
            package_prefixes: get(f, "packagePrefixes")
                .map_or_else(|_| Ok(vec![]), parse_string_vec)?,
            classes: get(f, "classes").map_or_else(|_| Ok(vec![]), parse_string_vec)?,
        })
    }

    Ok(ServiceConfiguration {
        id: get(parsed, "id")?.as_str().ok_or(())?.into(),
        allow: parse_filter_list(parsed, "allowList")?,
        deny: parse_filter_list(parsed, "denyList")?,
        sampling_snapshots_per_second: get(parsed, "sampling")
            .ok()
            .and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    Some(v.as_u32().ok_or(()))
                }
            })
            .transpose()?
            .unwrap_or(5000),
    })
}

pub fn parse(json: &str) -> ParseResult<LiveDebuggingData> {
    let parsed = json::parse(json).map_err(|_| ())?;
    parse_probe(&parsed)
        .map(LiveDebuggingData::Probe)
        .or(parse_service_configuration(&parsed).map(LiveDebuggingData::ServiceConfiguration))
}
