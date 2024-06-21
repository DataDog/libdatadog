// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::expr_defs::{
    BinaryComparison, CollectionMatch, CollectionSource, Condition, DslPart, NumberSource,
    Reference, StringComparison, StringSource, Value,
};
use crate::{
    CaptureConfiguration, DslString, EvaluateAt, FilterList, InBodyLocation, LiveDebuggingData,
    LogProbe, MetricKind, MetricProbe, Probe, ProbeCondition, ProbeTarget, ProbeType, ProbeValue,
    ServiceConfiguration, SpanDecorationProbe, SpanProbe, SpanProbeDecoration, SpanProbeTarget,
};
use serde::Deserialize;
use std::fmt::{Display, Formatter};

pub fn parse(json: &str) -> anyhow::Result<LiveDebuggingData> {
    let parsed: RawTopLevelItem = serde_json::from_str(json)?;
    fn err<T>(result: Result<T, (&'static str, RawExpr)>) -> anyhow::Result<T> {
        result.map_err(|(str, expr)| anyhow::format_err!("{str}: {expr}"))
    }
    Ok(match parsed.r#type {
        ContentType::ServiceConfiguration => {
            LiveDebuggingData::ServiceConfiguration(ServiceConfiguration {
                id: parsed.id,
                allow: parsed.allow.unwrap_or_default(),
                deny: parsed.deny.unwrap_or_default(),
                sampling_snapshots_per_second: parsed
                    .sampling
                    .map(|s| s.snapshots_per_second)
                    .unwrap_or(5000),
            })
        }
        probe_type => LiveDebuggingData::Probe({
            let mut probe = Probe {
                id: parsed.id,
                version: parsed.version.unwrap_or(0),
                language: parsed.language,
                tags: parsed.tags.unwrap_or_default(),
                target: {
                    let target = parsed
                        .r#where
                        .ok_or_else(|| anyhow::format_err!("Missing where for Probe"))?;
                    ProbeTarget {
                        type_name: target.type_name,
                        method_name: target.method_name,
                        source_file: target.source_file,
                        signature: target.signature,
                        lines: target.lines.unwrap_or(vec![]),
                        in_body_location: target.in_body_location.unwrap_or(InBodyLocation::None),
                    }
                },
                evaluate_at: parsed.evaluate_at.unwrap_or(EvaluateAt::Exit),
                probe: match probe_type {
                    ContentType::MetricProbe => ProbeType::Metric(MetricProbe {
                        kind: parsed
                            .kind
                            .ok_or_else(|| anyhow::format_err!("Missing kind for MetricProbe"))?,
                        name: parsed
                            .metric_name
                            .ok_or_else(|| anyhow::format_err!("Missing name for MetricProbe"))?,
                        value: ProbeValue(err(parsed
                            .value
                            .ok_or_else(|| anyhow::format_err!("Missing value for MetricProbe"))?
                            .json
                            .try_into())?),
                    }),
                    ContentType::LogProbe => ProbeType::Log(LogProbe {
                        segments: err(parsed
                            .segments
                            .ok_or_else(|| anyhow::format_err!("Missing segments for LogProbe"))?
                            .try_into())?,
                        when: ProbeCondition(
                            err(parsed.when.map(|expr| expr.json.try_into()).transpose())?
                                .unwrap_or(Condition::Always),
                        ),
                        capture: parsed.capture.unwrap_or_default(),
                        capture_snapshot: parsed.capture_snapshot.unwrap_or(false),
                        sampling_snapshots_per_second: parsed
                            .sampling
                            .map(|s| s.snapshots_per_second)
                            .unwrap_or(5000),
                    }),
                    ContentType::SpanProbe => ProbeType::Span(SpanProbe {}),
                    ContentType::SpanDecorationProbe => {
                        ProbeType::SpanDecoration(SpanDecorationProbe {
                            target: parsed.target_span.unwrap_or(SpanProbeTarget::Active),
                            decorations: {
                                let mut decorations = vec![];
                                for decoration in parsed.decorations.ok_or_else(|| {
                                    anyhow::format_err!(
                                        "Missing decorations for SpanDecorationProbe"
                                    )
                                })? {
                                    decorations.push(SpanProbeDecoration {
                                        condition: ProbeCondition(
                                            err(decoration
                                                .when
                                                .map(|expr| expr.json.try_into())
                                                .transpose())?
                                            .unwrap_or(Condition::Always),
                                        ),
                                        tags: {
                                            let mut tags = vec![];
                                            for tag in decoration.tags {
                                                tags.push((
                                                    tag.name,
                                                    err(tag.value.segments.try_into())?,
                                                ));
                                            }
                                            tags
                                        },
                                    })
                                }
                                decorations
                            },
                        })
                    }
                    _ => unreachable!(),
                },
            };
            // unconditional log probes always capture their entry context
            if matches!(
                probe.probe,
                ProbeType::Log(LogProbe {
                    when: ProbeCondition(Condition::Always),
                    ..
                })
            ) {
                probe.evaluate_at = EvaluateAt::Entry;
            }
            probe
        }),
    })
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ContentType {
    MetricProbe,
    LogProbe,
    SpanProbe,
    SpanDecorationProbe,
    ServiceConfiguration,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTopLevelItem {
    r#type: ContentType,
    id: String,
    version: Option<u64>,
    language: Option<String>,
    r#where: Option<ProbeWhere>,
    when: Option<Expression>,
    tags: Option<Vec<String>>,
    segments: Option<Vec<RawSegment>>,
    capture_snapshot: Option<bool>,
    capture: Option<CaptureConfiguration>,
    kind: Option<MetricKind>,
    decorations: Option<Vec<RawSpanProbeDecoration>>,
    metric_name: Option<String>,
    value: Option<Expression>,
    evaluate_at: Option<EvaluateAt>,
    allow: Option<FilterList>,
    deny: Option<FilterList>,
    sampling: Option<ServiceConfigurationSampling>,
    target_span: Option<SpanProbeTarget>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceConfigurationSampling {
    snapshots_per_second: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeWhere {
    type_name: Option<String>,
    method_name: Option<String>,
    source_file: Option<String>,
    signature: Option<String>,
    lines: Option<Vec<String>>,
    in_body_location: Option<InBodyLocation>,
}

#[derive(Deserialize)]
struct Expression {
    json: RawExpr,
}

#[derive(Deserialize)]
struct RawSpanProbeDecoration {
    when: Option<Expression>,
    tags: Vec<RawSpanProbeDecorationTag>,
}

#[derive(Deserialize)]
struct RawSpanProbeDecorationTag {
    name: String,
    value: RawSegments,
}

#[derive(Deserialize)]
struct RawSegments {
    segments: Vec<RawSegment>,
}

#[derive(Deserialize)]
struct RawSegmentString {
    str: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
#[serde(rename_all = "camelCase")]
enum RawSegment {
    Str(RawSegmentString),
    Expr(Expression),
}

impl TryInto<CollectionSource> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<CollectionSource, Self::Error> {
        let result: Result<CollectionSource, RawExpr> = self.try_into()?;
        result.or_else(|expr| Ok(CollectionSource::Reference(expr.try_into()?)))
    }
}

impl TryInto<Result<CollectionSource, RawExpr>> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<Result<CollectionSource, RawExpr>, Self::Error> {
        Ok(Ok(match self {
            RawExpr::Expr(Some(RawExprValue::Filter([source, cond]))) => {
                CollectionSource::FilterOperator(Box::new((
                    (*source).try_into()?,
                    (*cond).try_into()?,
                )))
            }
            expr => return Ok(Err(expr)),
        }))
    }
}

impl TryInto<Reference> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<Reference, Self::Error> {
        Ok(match self {
            RawExpr::Expr(Some(RawExprValue::Ref(identifier))) => {
                if identifier == "@it" {
                    Reference::IteratorVariable
                } else {
                    Reference::Base(identifier)
                }
            }
            RawExpr::Expr(Some(RawExprValue::Index([source, index]))) => {
                Reference::Index(Box::new(((*source).try_into()?, (*index).try_into()?)))
            }
            RawExpr::Expr(Some(RawExprValue::Getmember([source, member]))) => {
                Reference::Nested(Box::new(((*source).try_into()?, (*member).try_into()?)))
            }
            expr => return Err(("Found unexpected value for a reference", expr)),
        })
    }
}

impl TryInto<Condition> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<Condition, Self::Error> {
        let result: Result<Condition, RawExpr> = self.try_into()?;
        result.map_err(|expr| ("Found unexpected value for a condition", expr))
    }
}

impl TryInto<Result<Condition, RawExpr>> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<Result<Condition, RawExpr>, Self::Error> {
        Ok(Ok(match self {
            RawExpr::Bool(true) => Condition::Always,
            RawExpr::Bool(false) => Condition::Never,
            RawExpr::Expr(None) => Condition::Never,
            RawExpr::Expr(Some(RawExprValue::Or([a, b]))) => {
                Condition::Disjunction(Box::new(((*a).try_into()?, (*b).try_into()?)))
            }
            RawExpr::Expr(Some(RawExprValue::And([a, b]))) => {
                Condition::Conjunction(Box::new(((*a).try_into()?, (*b).try_into()?)))
            }
            RawExpr::Expr(Some(RawExprValue::Not(a))) => {
                Condition::Negation(Box::new((*a).try_into()?))
            }
            RawExpr::Expr(Some(RawExprValue::Eq([a, b]))) => Condition::BinaryComparison(
                (*a).try_into()?,
                BinaryComparison::Equals,
                (*b).try_into()?,
            ),
            RawExpr::Expr(Some(RawExprValue::Ne([a, b]))) => Condition::BinaryComparison(
                (*a).try_into()?,
                BinaryComparison::NotEquals,
                (*b).try_into()?,
            ),
            RawExpr::Expr(Some(RawExprValue::Gt([a, b]))) => Condition::BinaryComparison(
                (*a).try_into()?,
                BinaryComparison::GreaterThan,
                (*b).try_into()?,
            ),
            RawExpr::Expr(Some(RawExprValue::Ge([a, b]))) => Condition::BinaryComparison(
                (*a).try_into()?,
                BinaryComparison::GreaterOrEquals,
                (*b).try_into()?,
            ),
            RawExpr::Expr(Some(RawExprValue::Lt([a, b]))) => Condition::BinaryComparison(
                (*a).try_into()?,
                BinaryComparison::LowerThan,
                (*b).try_into()?,
            ),
            RawExpr::Expr(Some(RawExprValue::Le([a, b]))) => Condition::BinaryComparison(
                (*a).try_into()?,
                BinaryComparison::LowerOrEquals,
                (*b).try_into()?,
            ),
            RawExpr::Expr(Some(RawExprValue::StartsWith((source, value)))) => {
                Condition::StringComparison(
                    StringComparison::StartsWith,
                    (*source).try_into()?,
                    value,
                )
            }
            RawExpr::Expr(Some(RawExprValue::EndsWith((source, value)))) => {
                Condition::StringComparison(
                    StringComparison::EndsWith,
                    (*source).try_into()?,
                    value,
                )
            }
            RawExpr::Expr(Some(RawExprValue::Contains((source, value)))) => {
                Condition::StringComparison(
                    StringComparison::Contains,
                    (*source).try_into()?,
                    value,
                )
            }
            RawExpr::Expr(Some(RawExprValue::Matches((source, value)))) => {
                Condition::StringComparison(StringComparison::Matches, (*source).try_into()?, value)
            }
            RawExpr::Expr(Some(RawExprValue::Any([a, b]))) => Condition::CollectionMatch(
                CollectionMatch::Any,
                (*a).try_into()?,
                Box::new((*b).try_into()?),
            ),
            RawExpr::Expr(Some(RawExprValue::All([a, b]))) => Condition::CollectionMatch(
                CollectionMatch::All,
                (*a).try_into()?,
                Box::new((*b).try_into()?),
            ),
            RawExpr::Expr(Some(RawExprValue::Instanceof((source, name)))) => {
                Condition::Instanceof((*source).try_into()?, name)
            }
            RawExpr::Expr(Some(RawExprValue::IsUndefined(source))) => Condition::Negation(
                Box::new(Condition::IsDefinedReference((*source).try_into()?)),
            ),
            RawExpr::Expr(Some(RawExprValue::IsDefined(source))) => {
                Condition::IsDefinedReference((*source).try_into()?)
            }
            RawExpr::Expr(Some(RawExprValue::IsEmpty(source))) => {
                Condition::IsEmptyReference((*source).try_into()?)
            }
            expr => return Ok(Err(expr)),
        }))
    }
}

impl TryInto<NumberSource> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<NumberSource, Self::Error> {
        let result: Result<NumberSource, RawExpr> = self.try_into()?;
        result.or_else(|expr| Ok(NumberSource::Reference(expr.try_into()?)))
    }
}

impl TryInto<Result<NumberSource, RawExpr>> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<Result<NumberSource, RawExpr>, Self::Error> {
        Ok(Ok(match self {
            RawExpr::Number(num) => NumberSource::Number(num),
            RawExpr::Expr(Some(RawExprValue::Count(source))) => {
                NumberSource::CollectionSize((*source).try_into()?)
            }
            RawExpr::Expr(Some(RawExprValue::Len(source))) => {
                NumberSource::StringLength((*source).try_into()?)
            }
            expr => return Ok(Err(expr)),
        }))
    }
}

impl TryInto<StringSource> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<StringSource, Self::Error> {
        let result: Result<StringSource, RawExpr> = self.try_into()?;
        result.or_else(|expr| Ok(StringSource::Reference(expr.try_into()?)))
    }
}

impl TryInto<Result<StringSource, RawExpr>> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<Result<StringSource, RawExpr>, Self::Error> {
        Ok(Ok(match self {
            RawExpr::String(str) => StringSource::String(str),
            RawExpr::Expr(None) => StringSource::Null,
            RawExpr::Expr(Some(RawExprValue::Substring([source, start, end]))) => {
                StringSource::Substring(Box::new((
                    (*source).try_into()?,
                    (*start).try_into()?,
                    (*end).try_into()?,
                )))
            }
            expr => return Ok(Err(expr)),
        }))
    }
}

impl TryInto<Value> for RawExpr {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<Value, Self::Error> {
        let string: Result<StringSource, _> = self.try_into()?;
        Ok(match string {
            Ok(string) => Value::String(string),
            Err(expr) => {
                let num: Result<NumberSource, _> = expr.try_into()?;
                match num {
                    Ok(num) => Value::Number(num),
                    Err(expr) => {
                        let num: Result<Condition, _> = expr.try_into()?;
                        match num {
                            Ok(num) => Value::Bool(Box::new(num)),
                            Err(expr) => Value::String(StringSource::Reference(expr.try_into()?)),
                        }
                    }
                }
            }
        })
    }
}

impl TryInto<DslString> for Vec<RawSegment> {
    type Error = (&'static str, RawExpr);

    fn try_into(self) -> Result<DslString, Self::Error> {
        let mut dsl_parts = vec![];
        for segment in self {
            dsl_parts.push(match segment {
                RawSegment::Str(str) => DslPart::String(str.str),
                RawSegment::Expr(expr) => match expr.json.try_into()? {
                    Ok(reference) => DslPart::Ref(reference),
                    Err(expr) => DslPart::Value(expr.try_into()?),
                },
            });
        }
        Ok(DslString(dsl_parts))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawExpr {
    Bool(bool),
    String(String),
    Number(f64),
    Expr(Option<RawExprValue>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum RawExprValue {
    Ref(String),
    Any([Box<RawExpr>; 2]),
    All([Box<RawExpr>; 2]),
    Or([Box<RawExpr>; 2]),
    And([Box<RawExpr>; 2]),
    Eq([Box<RawExpr>; 2]),
    Ne([Box<RawExpr>; 2]),
    Lt([Box<RawExpr>; 2]),
    Le([Box<RawExpr>; 2]),
    Gt([Box<RawExpr>; 2]),
    Ge([Box<RawExpr>; 2]),
    Contains((Box<RawExpr>, String)),
    Matches((Box<RawExpr>, String)),
    StartsWith((Box<RawExpr>, String)),
    EndsWith((Box<RawExpr>, String)),
    Filter([Box<RawExpr>; 2]),
    Getmember([Box<RawExpr>; 2]),
    Not(Box<RawExpr>),
    Count(Box<RawExpr>),
    IsEmpty(Box<RawExpr>),
    IsDefined(Box<RawExpr>),
    IsUndefined(Box<RawExpr>),
    Len(Box<RawExpr>),
    Instanceof((Box<RawExpr>, String)),
    Index([Box<RawExpr>; 2]),
    Substring([Box<RawExpr>; 3]),
}

impl Display for RawExpr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RawExpr::Bool(true) => f.write_str("true"),
            RawExpr::Bool(false) => f.write_str("false"),
            RawExpr::String(s) => write!(f, "\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            RawExpr::Number(n) => n.fmt(f),
            RawExpr::Expr(None) => f.write_str("null"),
            RawExpr::Expr(Some(RawExprValue::Ref(id))) => id.fmt(f),
            RawExpr::Expr(Some(RawExprValue::Any([a, b]))) => write!(f, "any({a}, {b})"),
            RawExpr::Expr(Some(RawExprValue::All([a, b]))) => write!(f, "all({a}, {b})"),
            RawExpr::Expr(Some(RawExprValue::Or([a, b]))) => write!(f, "{a} || {b}"),
            RawExpr::Expr(Some(RawExprValue::And([a, b]))) => write!(f, "{a} && {b}"),
            RawExpr::Expr(Some(RawExprValue::Eq([a, b]))) => write!(f, "{a} == {b}"),
            RawExpr::Expr(Some(RawExprValue::Ne([a, b]))) => write!(f, "{a} != {b}"),
            RawExpr::Expr(Some(RawExprValue::Lt([a, b]))) => write!(f, "{a} < {b}"),
            RawExpr::Expr(Some(RawExprValue::Le([a, b]))) => write!(f, "{a} <= {b}"),
            RawExpr::Expr(Some(RawExprValue::Gt([a, b]))) => write!(f, "{a} > {b}"),
            RawExpr::Expr(Some(RawExprValue::Ge([a, b]))) => write!(f, "{a} >= {b}"),
            RawExpr::Expr(Some(RawExprValue::Contains((src, str)))) => {
                write!(f, "contains({src}, {str})")
            }
            RawExpr::Expr(Some(RawExprValue::Matches((src, str)))) => {
                write!(f, "matches({src}, {str})")
            }
            RawExpr::Expr(Some(RawExprValue::StartsWith((src, str)))) => {
                write!(f, "startsWith({src}, {str})")
            }
            RawExpr::Expr(Some(RawExprValue::EndsWith((src, str)))) => {
                write!(f, "endsWith({src}, {str})")
            }
            RawExpr::Expr(Some(RawExprValue::Filter([a, b]))) => write!(f, "filter({a}, {b})"),
            RawExpr::Expr(Some(RawExprValue::Getmember([a, b]))) => {
                if let RawExpr::String(ref s) = **b {
                    write!(f, "{a}.{s}")
                } else {
                    write!(f, "{a}.{b}")
                }
            }
            RawExpr::Expr(Some(RawExprValue::Not(a))) => write!(f, "!{a}"),
            RawExpr::Expr(Some(RawExprValue::Count(a))) => write!(f, "count({a})"),
            RawExpr::Expr(Some(RawExprValue::IsEmpty(a))) => write!(f, "isEmpty({a})"),
            RawExpr::Expr(Some(RawExprValue::IsDefined(a))) => write!(f, "isDefined({a})"),
            RawExpr::Expr(Some(RawExprValue::IsUndefined(a))) => write!(f, "isUndefined({a})"),
            RawExpr::Expr(Some(RawExprValue::Len(a))) => write!(f, "len({a})"),
            RawExpr::Expr(Some(RawExprValue::Instanceof((src, class)))) => {
                write!(f, "instanceof({src}, {class})")
            }
            RawExpr::Expr(Some(RawExprValue::Index([a, b]))) => write!(f, "{a}[{b}]"),
            RawExpr::Expr(Some(RawExprValue::Substring([src, start, end]))) => {
                write!(f, "substring({src}, {start}, {end})")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        parse_json, CaptureConfiguration, EvaluateAt, LiveDebuggingData, LogProbe, MetricKind,
        MetricProbe, Probe, ProbeType, SpanDecorationProbe, SpanProbeTarget,
    };

    #[test]
    fn test_spandecoration_probe_deserialize() {
        let json = r#"
{
  "id": "2142910d-d2ff-4679-85cc-bfc317d74e8f",
  "version": 42,
  "type": "SPAN_DECORATION_PROBE",
  "language": "java",
  "tags": ["foo:bar", "baz:baaz"],
  "where": {
    "typeName": "VetController",
    "methodName": "showVetList"
  },
  "targetSpan": "ACTIVE",
  "decorations": [{
    "when": {
      "dsl": "field1 > 10",
      "json": {
        "gt": [{"ref": "field1"}, 10]
      }
    },
    "tags": [{
      "name": "transactions",
      "value": {
        "template": "{transactions.id}-{filter(transactions, startsWith(@it.status, 2))}",
        "segments": [{
          "dsl": "transactions.id",
          "json": {
            "getmember": [{"ref": "transactions"}, "id"]
          }
        }, {
          "str": "-"
        }, {
          "dsl": "filter(transactions, startsWith(@it[\"status\"], 2))",
          "json": {
            "filter": [{"ref": "transaction"}, {
              "startsWith": [
                {
                  "index": [{"ref": "@it"}, "status"]
                },
                "2"
              ]
            }]
          }
        }]
      }
    }]
 }, {
    "when": {
      "dsl": "!(obj == null)",
      "json": {
        "not": {"eq": [{"ref": "obj"}, null]}
      }
    },
    "tags": [{
      "name": "value",
      "value": {
        "template": "{substring(arr[obj.key], 0, len(@return))}",
        "segments": [{
          "dsl": "obj.value",
          "json": {
            "substring": [
              {"index": [{"ref": "arr"}, {"getmember": [{"ref": "obj"}, "key"]}]},
              0,
              {"len": {"ref": "@return"}}
            ]
          }
        }]
      }
    }]
  }]
}
"#;

        let parsed = parse_json(json).unwrap();
        if let LiveDebuggingData::Probe(Probe {
            id,
            version,
            language,
            tags,
            target,
            evaluate_at,
            probe:
                ProbeType::SpanDecoration(SpanDecorationProbe {
                    target: probe_target,
                    decorations,
                }),
        }) = parsed
        {
            assert_eq!(id, "2142910d-d2ff-4679-85cc-bfc317d74e8f");
            assert_eq!(version, 42);
            assert_eq!(language, Some("java".to_string()));
            assert_eq!(tags, vec!["foo:bar".to_string(), "baz:baaz".to_string()]);
            assert_eq!(target.method_name, Some("showVetList".to_string()));
            assert!(matches!(evaluate_at, EvaluateAt::Exit));
            assert!(matches!(probe_target, SpanProbeTarget::Active));
            assert_eq!(decorations[0].condition.to_string(), "field1 > 10");
            let (tag, expr) = &decorations[0].tags[0];
            assert_eq!(tag, "transactions");
            assert_eq!(
                expr.to_string(),
                r#"{transactions.id}-{filter(transaction, startsWith(@it["status"], 2))}"#
            );
            assert_eq!(decorations[1].condition.to_string(), "!(obj == null)");
            let (tag, expr) = &decorations[1].tags[0];
            assert_eq!(tag, "value");
            assert_eq!(
                expr.to_string(),
                r#"{substring(arr[obj.key], 0, len(@return))}"#
            );
        } else {
            unreachable!();
        }
    }

    #[test]
    fn test_log_probe_deserialize() {
        let json = r#"
{
  "id": "2142910d-d2ff-4679-85cc-bfc317d74e8f",
  "version": 42,
  "type": "LOG_PROBE",
  "language": "java",
  "tags": ["foo:bar", "baz:baaz"],
  "evaluateAt": "ENTRY",
  "where": {
    "typeName": "VetController",
    "methodName": "showVetList"
  },
  "template": "Id of transaction: {transactionId}",
  "segments": [{
      "str": "Id of transaction: "
    }, {
      "dsl": "transactionId",
      "json": {"ref": "transactionId"}
    }
  ],
  "captureSnapshot": true,
  "when": {
    "dsl": "(@duration > 500 && (!(isDefined(myField)) && localVar1.field1.field2 != 15)) || (isEmpty(this.collectionField) || any(this.collectionField, { isEmpty(@it.name) }))",
    "json": {
      "or": [{
        "and": [{
          "gt": [{"ref": "@duration"}, 500]
        }, {
          "and": [{
            "not": {
              "isDefined": {"ref": "myField"}
            }
          }, {
            "ne": [
              {
                "getmember": [
                  {
                    "getmember": [{"ref": "localVar1"}, "field1"]
                  },
                  "field2"
                ]
              },
              15
            ]
          }]
        }]
      }, {
        "or": [{
          "isEmpty": {"ref": "this.collectionField"}
        }, {
          "any": [{
            "ref": "this.collectionField"
          }, {
             "isEmpty": { "ref": "@it.name" }
          }]
        }]
      }]
    }
  },
  "capture": {
    "maxReferenceDepth": 3,
    "maxCollectionSize": 1,
    "maxLength": 255,
    "maxFieldCount": 20
  },
  "sampling": {
    "snapshotsPerSecond": 10
  }
}
"#;

        let parsed = parse_json(json).unwrap();
        if let LiveDebuggingData::Probe(Probe {
            evaluate_at,
            probe:
                ProbeType::Log(LogProbe {
                    segments,
                    when,
                    capture:
                        CaptureConfiguration {
                            max_reference_depth,
                            max_collection_size,
                            max_length,
                            max_field_count,
                        },
                    capture_snapshot,
                    sampling_snapshots_per_second,
                }),
            ..
        }) = parsed
        {
            assert!(matches!(evaluate_at, EvaluateAt::Entry));
            assert_eq!(segments.to_string(), "Id of transaction: {transactionId}");
            assert_eq!(when.to_string(), "(@duration > 500 && !isDefined(myField) && localVar1.field1.field2 != 15) || isEmpty(this.collectionField) || any(this.collectionField, isEmpty(@it.name))");
            assert_eq!(max_reference_depth, 3);
            assert_eq!(max_collection_size, 1);
            assert_eq!(max_length, 255);
            assert_eq!(max_field_count, 20);
            assert_eq!(sampling_snapshots_per_second, 10);
            assert_eq!(capture_snapshot, true);
        } else {
            unreachable!();
        }
    }

    #[test]
    fn test_metric_probe_deserialize() {
        let json = r#"
{
  "id": "2142910d-d2ff-4679-85cc-bfc317d74e8f",
  "version": 42,
  "type": "METRIC_PROBE",
  "language": "java",
  "tags": ["foo:bar", "baz:baaz"],
  "where": {
    "typeName": "VetController",
    "methodName": "showVetList"
  },
  "evaluateAt": "EXIT",
  "metricName": "showVetList.callcount",
  "kind": "COUNT",
  "value": {
    "dsl": "arg",
    "json": {"ref": "arg"}
  }
}
"#;

        let parsed = parse_json(json).unwrap();
        if let LiveDebuggingData::Probe(Probe {
            evaluate_at,
            probe: ProbeType::Metric(MetricProbe { kind, name, value }),
            ..
        }) = parsed
        {
            assert!(matches!(evaluate_at, EvaluateAt::Exit));
            assert!(matches!(kind, MetricKind::Count));
            assert_eq!(name, "showVetList.callcount");
            assert_eq!(value.to_string(), "arg");
        } else {
            unreachable!();
        }
    }
}
