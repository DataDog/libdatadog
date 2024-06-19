use anyhow::Context;
use crate::expr_defs::{Condition, StringSource, Value};
use crate::parse_json_expr::{parse_condition, parse_segments, parse_value};
use crate::parse_util::{get, try_get};
use crate::{
    Capture, EvaluateAt, FilterList, InBodyLocation, LiveDebuggingData, LogProbe, MetricKind,
    MetricProbe, Probe, ProbeCondition, ProbeTarget, ProbeType, ProbeValue, ServiceConfiguration,
    SpanDecorationProbe, SpanProbe, SpanProbeDecoration, SpanProbeTarget,
};
use json::JsonValue;

fn parse_string_vec(array: &JsonValue) -> anyhow::Result<Vec<String>> {
    let mut vec = vec![];
    if !array.is_array() {
        anyhow::bail!("Tried to get Vec from non-array");
    }
    for value in array.members() {
        vec.push(value.as_str().ok_or_else(|| anyhow::format_err!("Failed to get string from array value"))?.to_string());
    }
    Ok(vec)
}

fn parse_probe(parsed: &JsonValue) -> anyhow::Result<Probe> {
    let mut tags = vec![];
    if let Some(json_tags) = try_get(parsed, "tags") {
        tags = parse_string_vec(json_tags).context("parsing tags")?;
    }

    let target = get(parsed, "where")?;
    let lines = if let Some(lines) = try_get(target, "lines") {
        parse_string_vec(get(lines, "where").context("parsing lines")?).context("parsing lines")?
    } else {
        vec![]
    };

    let target_get = |name: &str| -> anyhow::Result<Option<String>> {
        try_get(target, name)
            .and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    Some(v.as_str().map(ToString::to_string).ok_or_else(|| anyhow::format_err!("Failed getting string for {name}")))
                }
            })
            .transpose()
    };
    let probe = match get(parsed, "type")?.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from type"))? {
        "METRIC_PROBE" => ProbeType::Metric(MetricProbe {
            kind: match get(parsed, "kind")?.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from METRIC_PROBE.kind"))? {
                "COUNT" => MetricKind::Count,
                "GAUGE" => MetricKind::Gauge,
                "HISTOGRAM" => MetricKind::Histogram,
                "DISTRIBUTION" => MetricKind::Distribution,
                kind => anyhow::bail!("{kind} is not a valid METRIC_PROBE.kind"),
            },
            name: get(parsed, "metricName")?.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from metricName"))?.to_string(),
            value: ProbeValue(
                try_get(parsed, "value")
                    .map(|v| {
                        if v.is_null() {
                            Ok(Value::String(StringSource::Null))
                        } else {
                            parse_value(v)
                        }
                    })
                    .transpose()?
                    .unwrap_or(Value::String(StringSource::Null)),
            ),
        }),
        "LOG_PROBE" => ProbeType::Log(LogProbe {
            segments: parse_segments(get(parsed, "segments")?).context("while parsing LOG_PROBE.segments")?,
            when: ProbeCondition(
                try_get(parsed, "when")
                    .map(|v| parse_condition(v).context("while parsing LOG_PROBE.when"))
                    .unwrap_or(Ok(Condition::Always))?,
            ),
            capture: {
                let mut capture = Capture::default();
                if let Some(v) = try_get(parsed, "capture") {
                    if !v.is_null() {
                        if let Some(max_reference_depth) = try_get(v, "maxReference_depth") {
                            capture.max_reference_depth = max_reference_depth.as_u32().ok_or_else(|| anyhow::format_err!("Failed getting u32 from LOG_PROBE.capture.maxReferenceDepth"))?;
                        }
                        if let Some(max_collection_size) = try_get(v, "maxCollectionSize") {
                            capture.max_collection_size = max_collection_size.as_u32().ok_or_else(|| anyhow::format_err!("Failed getting u32 from LOG_PROBE.capture.maxCollectionSize"))?;
                        }
                        if let Some(max_length) = try_get(v, "maxLength") {
                            capture.max_length = max_length.as_u32().ok_or_else(|| anyhow::format_err!("Failed getting u32 from LOG_PROBE.capture.maxLength"))?;
                        }
                        if let Some(max_field_depth) = try_get(v, "maxFieldDepth") {
                            capture.max_field_depth = max_field_depth.as_u32().ok_or_else(|| anyhow::format_err!("Failed getting u32 from LOG_PROBE.capture.maxFieldDepth"))?;
                        }
                    }
                }
                capture
            },
            sampling_snapshots_per_second: try_get(parsed, "sampling")
                .and_then(|v| {
                    if v.is_null() {
                        None
                    } else {
                        Some(v.as_u32().ok_or_else(|| anyhow::format_err!("Failed getting u32 from LOG_PROBE.sampling")))
                    }
                })
                .transpose()?
                .unwrap_or(5000),
        }),
        "SPAN_PROBE" => ProbeType::Span(SpanProbe {}),
        "SPAN_DECORATION_PROBE" => ProbeType::SpanDecoration(SpanDecorationProbe {
            target: match try_get(parsed, "targetSpan").map_or(Ok("ACTIVE"), |v| v.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from SPAN_DECORATION_PROBE.targetSpan")))? {
                "ACTIVE" => SpanProbeTarget::Active,
                "ROOT" => SpanProbeTarget::Root,
                target => anyhow::bail!("{target} is not a valid SPAN_DECORATION_PROBE.targetSpan"),
            },
            decorations: {
                let mut vec = vec![];
                let decorations = get(parsed, "decorations").context("on SPAN_DECORATIONS_PROBE")?;
                if !decorations.is_array() {
                    anyhow::bail!("SPAN_DECORATIONS_PROBE.decorations is not an array");
                }
                for decoration in decorations.members() {
                    let tags = get(decoration, "tags").context("on SPAN_DECORATIONS_PROBE.decorations")?;
                    if !tags.is_array() {
                        anyhow::bail!("SPAN_DECORATIONS_PROBE.decorations.tags is not an array");
                    }
                    let mut tagvec = vec![];
                    for tag in tags.members() {
                        let name = get(tag, "name").context("on SPAN_DECORATIONS_PROBE.decorations[].tags[]")?.as_str().ok_or_else(|| anyhow::format_err!("SPAN_DECORATIONS_PROBE.decorations.tags[].name is not a string"))?.to_string();
                        let value = parse_segments(get(tag, "value")?).context("while parsing SPAN_DECORATIONS_PROBE.decorations[].tags[].value")?;
                        tagvec.push((name, value));
                    }
                    let condition = try_get(decoration, "when")
                        .map(|v| {
                            if v.is_null() {
                                Ok(Condition::Always)
                            } else {
                                parse_condition(v).context("parsing the condition of SPAN_DECORATION_PROBE.decorations[].when")
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
        r#type => anyhow::bail!("Unknown probe type {type}"),
    };

    Ok(Probe {
        id: get(parsed, "id")?.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from id"))?.into(),
        version: get(parsed, "version")?.as_u64().unwrap_or(0),
        language: get(parsed, "language")?.as_str().map(ToString::to_string),
        tags,
        target: ProbeTarget {
            type_name: target_get("typeName")?,
            method_name: target_get("methodName")?,
            source_file: target_get("sourceFile")?,
            signature: target_get("signature")?,
            lines,
            in_body_location: match target_get("inBodyLocation")? {
                None => InBodyLocation::None,
                Some(string) => match string.as_str() {
                    "START" => InBodyLocation::Start,
                    "END" => InBodyLocation::End,
                    location => anyhow::bail!("{location} is not a valid inBodyLocation"),
                },
            },
        },
        evaluate_at: match get(parsed, "evaluateAt")?.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from evaluateAt"))? {
            "ENTRY" => EvaluateAt::Entry,
            "EXIT" => EvaluateAt::Exit,
            eval_at => anyhow::bail!("{eval_at} is not a valid evaluateAt"),
        },
        probe,
    })
}

fn parse_service_configuration(parsed: &JsonValue) -> anyhow::Result<ServiceConfiguration> {
    fn parse_filter_list(parsed: &JsonValue, key: &str) -> anyhow::Result<FilterList> {
        let f = get(parsed, key)?;
        Ok(FilterList {
            package_prefixes: try_get(f, "packagePrefixes")
                .map_or(Ok(vec![]), parse_string_vec).map_err(|e| e.context(format!("while parsing {key}.packagePrefixes")))?,
            classes: try_get(f, "classes").map_or(Ok(vec![]), parse_string_vec)
                .map_err(|e| e.context(format!("while parsing {key}.classes")))?,
        })
    }

    Ok(ServiceConfiguration {
        id: get(parsed, "id")?.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from id"))?.into(),
        allow: parse_filter_list(parsed, "allowList")?,
        deny: parse_filter_list(parsed, "denyList")?,
        sampling_snapshots_per_second: try_get(parsed, "sampling")
            .and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    Some(v.as_u32().ok_or_else(|| anyhow::format_err!("Failed getting u32 from sampling")))
                }
            })
            .transpose()?
            .unwrap_or(5000),
    })
}

pub fn parse(json: &str) -> anyhow::Result<LiveDebuggingData> {
    let parsed = json::parse(json)?;
    Ok(match get(&parsed, "type")?.as_str().ok_or_else(|| anyhow::format_err!("Failed getting string from type"))? {
        "SERVICE_CONFIGURATION" => LiveDebuggingData::Probe(parse_probe(&parsed).context("while parsing probe")?),
        _ => LiveDebuggingData::ServiceConfiguration(parse_service_configuration(&parsed).context("While parsing service configuration")?),
    })
}
