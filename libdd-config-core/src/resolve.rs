// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::mem;

use indexmap::{IndexMap, map::Entry as MapEntry};
use serde::{Deserialize, Serialize};

use crate::{
    ConfigSchema, Error, TransformContext, TransformResult, Value, ValueKind, schema::Rule,
    transforms::Binding,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Default,
    LocalStableConfig,
    EnvVar,
    OtelEnvVar,
    FleetStableConfig,
    Code,
    RemoteConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum InputValue {
    Raw(String),
    Typed(Value),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub name: String,
    pub value: InputValue,
    #[serde(default)]
    pub config_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Layer {
    pub origin: Origin,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub skip_empty: bool,
    #[serde(default)]
    pub alias_order: AliasOrder,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasOrder {
    #[default]
    AsProvided,
    AsDeclared,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ResolveInputs {
    /// Sources are applied from lowest to highest precedence.
    pub layers: Vec<Layer>,
    /// Record attempts for telemetry, including overridden sources.
    pub report_sources: bool,
    /// Resolve only the supplied layers, for callers that already have defaults.
    pub skip_defaults: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolvedValue {
    pub value: Value,
    pub origin: Origin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Setting<'a> {
    pub name: &'a str,
    pub property: &'a str,
    pub resolved: Option<&'a ResolvedValue>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum DiagnosticKind {
    UnknownOption,
    InvalidValue,
    DisallowedValue,
    TransformRejected(Option<String>),
    TransformWarning(String),
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub name: String,
    /// The supplied name when it differs from the canonical name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_name: Option<String>,
    pub origin: Origin,
    pub problem: DiagnosticKind,
}

#[derive(Debug, Serialize)]
pub struct SourceEvent {
    pub name: String,
    /// The supplied name when it differs from the canonical name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_name: Option<String>,
    pub origin: Origin,
    /// The configuration ID supplied for this attempt, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_id: Option<String>,
    pub input: Option<InputValue>,
    /// The parsed input when validation or transformation changed or rejected it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<Value>,
    pub value: Option<Value>,
}

#[derive(Debug)]
pub struct ResolvedConfig {
    schema: ConfigSchema,
    values: Vec<Option<ResolvedValue>>,
    diagnostics: Vec<Diagnostic>,
    sources: Vec<SourceEvent>,
}

impl ResolvedConfig {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ResolvedValue> {
        self.values[self.schema.0.names.get(name)?.slot].as_ref()
    }

    #[must_use = "iterate over the settings to read the resolved values"]
    pub fn settings(&self) -> impl ExactSizeIterator<Item = Setting<'_>> {
        self.schema
            .0
            .rules
            .iter()
            .zip(&self.values)
            .map(|(rule, value)| Setting {
                name: &rule.name,
                property: &rule.property,
                resolved: value.as_ref(),
            })
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn source_events(&self) -> &[SourceEvent] {
        &self.sources
    }
}

#[derive(Debug, Serialize)]
pub struct TransformRequest {
    pub slot: u32,
    pub key: String,
    pub origin: Origin,
    pub value: Value,
}

#[derive(Debug)]
pub enum Resolution {
    Complete(ResolvedConfig),
    Pending(PendingResolution),
}

#[derive(Debug)]
pub struct PendingResolution {
    snapshot: ResolvedConfig,
    requests: Vec<TransformRequest>,
    deferred: Vec<Deferred>,
    report_sources: bool,
}

#[derive(Debug)]
enum Deferred {
    Ready(Write, Decision),
    Host(Write, ValueKind),
    Diagnostic(Diagnostic),
}

#[derive(Debug)]
struct Write {
    slot: usize,
    origin: Origin,
    input_name: Option<String>,
    config_id: Option<String>,
    input: Option<InputValue>,
    parsed: Option<Value>,
}

#[derive(Debug)]
enum Outcome {
    Ready(Decision),
    Host(ValueKind),
}

#[derive(Debug)]
struct Decision {
    value: Option<Value>,
    diagnostic: Option<DiagnosticKind>,
}

impl PendingResolution {
    #[must_use]
    pub fn requests(&self) -> &[TransformRequest] {
        &self.requests
    }

    /// Feed outcomes back in request order.
    ///
    /// # Errors
    /// Returns an error if the host supplies an incorrect count or an incompatible value.
    pub fn resume(mut self, outcomes: Vec<TransformResult>) -> Result<ResolvedConfig, Error> {
        if outcomes.len() != self.requests.len() {
            return Err(Error::TransformCount {
                expected: self.requests.len(),
                actual: outcomes.len(),
            });
        }

        let mut outcomes = outcomes.into_iter();
        for deferred in mem::take(&mut self.deferred) {
            match deferred {
                Deferred::Ready(write, decision) => self.apply(write, decision),
                Deferred::Host(write, kind) => {
                    let rule = &self.snapshot.schema.0.rules[write.slot];
                    let result = outcomes.next().ok_or(Error::TransformCount {
                        expected: self.requests.len(),
                        actual: 0,
                    })?;
                    let decision = validate_transform(rule, kind, result)?;
                    self.apply(write, decision);
                }
                Deferred::Diagnostic(diagnostic) => self.snapshot.diagnostics.push(diagnostic),
            }
        }
        Ok(self.snapshot)
    }

    fn add(&mut self, write: Write, outcome: Outcome) {
        match outcome {
            Outcome::Ready(decision) if self.requests.is_empty() => self.apply(write, decision),
            Outcome::Ready(decision) => self.deferred.push(Deferred::Ready(write, decision)),
            Outcome::Host(kind) => self.deferred.push(Deferred::Host(write, kind)),
        }
    }

    fn add_diagnostic(&mut self, diagnostic: Diagnostic) {
        if self.requests.is_empty() {
            self.snapshot.diagnostics.push(diagnostic);
        } else {
            self.deferred.push(Deferred::Diagnostic(diagnostic));
        }
    }

    fn apply(&mut self, write: Write, Decision { value, diagnostic }: Decision) {
        let rule = &self.snapshot.schema.0.rules[write.slot];
        let input_name = write.input_name.as_ref().filter(|name| **name != rule.name);
        if let Some(problem) = diagnostic {
            self.snapshot.diagnostics.push(Diagnostic {
                name: rule.name.clone(),
                input_name: input_name.cloned(),
                origin: write.origin,
                problem,
            });
        }

        if self.report_sources && !rule.sensitive {
            self.snapshot.sources.push(SourceEvent {
                name: rule.name.clone(),
                input_name: input_name.cloned(),
                origin: write.origin,
                config_id: write.config_id.clone(),
                input: write.input,
                parsed: write.parsed.filter(|parsed| Some(parsed) != value.as_ref()),
                value: value.clone(),
            });
        }

        if let Some(value) = value {
            self.snapshot.values[write.slot] = Some(ResolvedValue {
                value,
                origin: write.origin,
                input_name: write.input_name,
                config_id: write.config_id,
            });
        }
    }
}

impl ConfigSchema {
    /// Resolve one configuration snapshot using the sources captured by the caller.
    ///
    /// # Errors
    /// Returns an error if a transform returns a value outside its declared output kind.
    pub fn resolve(&self, inputs: ResolveInputs) -> Result<Resolution, Error> {
        let mut pending = PendingResolution {
            snapshot: ResolvedConfig {
                schema: self.clone(),
                values: vec![None; self.0.rules.len()],
                diagnostics: Vec::new(),
                sources: Vec::new(),
            },
            requests: Vec::new(),
            deferred: Vec::new(),
            report_sources: inputs.report_sources,
        };

        if !inputs.skip_defaults {
            for (slot, rule) in self.0.rules.iter().enumerate() {
                let origin = Origin::Default;
                let outcome = match &rule.default {
                    Some(value) => self.evaluate(rule, value.clone(), origin, &mut pending)?,
                    None => Outcome::ready(None),
                };
                pending.add(
                    Write {
                        slot,
                        origin,
                        input_name: None,
                        config_id: None,
                        input: None,
                        parsed: rule
                            .parsed_for_source(rule.default.as_ref(), inputs.report_sources),
                    },
                    outcome,
                );
            }
        }

        for layer in inputs.layers {
            if layer.origin == Origin::Code {
                for entry in layer.entries {
                    if let Some(&slot) = self.0.options.get(&entry.name) {
                        self.resolve_entry(slot, layer.origin, entry, &mut pending)?;
                    } else {
                        pending.add_diagnostic(Diagnostic {
                            name: entry.name,
                            input_name: None,
                            origin: layer.origin,
                            problem: DiagnosticKind::UnknownOption,
                        });
                    }
                }
            } else {
                let mut selected = IndexMap::new();
                for entry in layer.entries {
                    if layer.skip_empty
                        && matches!(&entry.value, InputValue::Raw(value) if value.is_empty())
                    {
                        continue;
                    }
                    if let Some(name) = self.0.names.get(&entry.name) {
                        let rank = name.alias_rank;
                        match selected.entry(name.slot) {
                            MapEntry::Vacant(position) => {
                                position.insert((rank, entry));
                            }
                            MapEntry::Occupied(mut position) => {
                                if rank == 0
                                    || (layer.alias_order == AliasOrder::AsDeclared
                                        && rank < position.get().0)
                                {
                                    position.insert((rank, entry));
                                }
                            }
                        }
                    }
                }
                for (slot, (_, entry)) in selected {
                    self.resolve_entry(slot, layer.origin, entry, &mut pending)?;
                }
            }
        }

        if pending.requests.is_empty() {
            Ok(Resolution::Complete(pending.snapshot))
        } else {
            Ok(Resolution::Pending(pending))
        }
    }

    fn resolve_entry(
        &self,
        slot: usize,
        origin: Origin,
        entry: Entry,
        pending: &mut PendingResolution,
    ) -> Result<(), Error> {
        let rule = &self.0.rules[slot];
        let input = (pending.report_sources && !rule.sensitive).then(|| entry.value.clone());
        let parsed = match entry.value {
            InputValue::Raw(raw) => self.0.policy.parse(rule.kind, rule.parse_options, &raw),
            InputValue::Typed(passed) => {
                self.0.policy.coerce(rule.kind, rule.parse_options, passed)
            }
        };
        let parsed_for_source = rule.parsed_for_source(parsed.as_ref(), pending.report_sources);
        let outcome = match parsed {
            Some(value) => self.evaluate(rule, value, origin, pending)?,
            None => Outcome::rejected(DiagnosticKind::InvalidValue),
        };
        pending.add(
            Write {
                slot,
                origin,
                input_name: Some(entry.name),
                config_id: entry.config_id,
                input,
                parsed: parsed_for_source,
            },
            outcome,
        );
        Ok(())
    }

    fn evaluate(
        &self,
        rule: &Rule,
        value: Value,
        origin: Origin,
        pending: &mut PendingResolution,
    ) -> Result<Outcome, Error> {
        if (origin != Origin::Default || rule.validate_default)
            && let Some(pattern) = &rule.allowed
            && !self.0.policy.pattern_matches(pattern, &value)
        {
            return Ok(Outcome::rejected(DiagnosticKind::DisallowedValue));
        }

        let Some(transform) = rule.transform else {
            return Ok(Outcome::ready(Some(value)));
        };
        let context = TransformContext {
            key: &rule.name,
            origin,
        };
        match transform.binding {
            Binding::Function(function) => {
                validate_transform(rule, transform.output, function(value, context))
                    .map(Outcome::Ready)
            }
            Binding::Host(slot) => {
                pending.requests.push(TransformRequest {
                    slot,
                    key: rule.name.clone(),
                    origin,
                    value,
                });
                Ok(Outcome::Host(transform.output))
            }
        }
    }
}

impl Rule {
    fn parsed_for_source(&self, value: Option<&Value>, report_sources: bool) -> Option<Value> {
        if report_sources && !self.sensitive && (self.transform.is_some() || self.allowed.is_some())
        {
            value.cloned()
        } else {
            None
        }
    }
}

impl Outcome {
    const fn ready(value: Option<Value>) -> Self {
        Self::Ready(Decision {
            value,
            diagnostic: None,
        })
    }

    const fn rejected(diagnostic: DiagnosticKind) -> Self {
        Self::Ready(Decision {
            value: None,
            diagnostic: Some(diagnostic),
        })
    }
}

fn validate_transform(
    rule: &Rule,
    kind: ValueKind,
    result: TransformResult,
) -> Result<Decision, Error> {
    match result {
        TransformResult::Accept { value, warning } if value.matches(kind) => Ok(Decision {
            value: Some(value),
            diagnostic: warning
                .filter(|_| !rule.sensitive)
                .map(DiagnosticKind::TransformWarning),
        }),
        TransformResult::Accept { .. } => Err(Error::InvalidTransformResult(rule.name.clone())),
        TransformResult::Reject { diagnostic } => Ok(Decision {
            value: None,
            diagnostic: Some(DiagnosticKind::TransformRejected(
                diagnostic.filter(|_| !rule.sensitive),
            )),
        }),
    }
}
