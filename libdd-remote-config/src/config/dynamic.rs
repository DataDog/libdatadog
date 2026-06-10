// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Default, Serialize))]
pub struct DynamicConfigTarget {
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct DynamicConfigFile {
    pub action: String,
    #[serde(default)]
    pub service_target: Option<DynamicConfigTarget>,
    pub lib_config: DynamicConfig,
}

impl DynamicConfigFile {
    /// Returns the priority of this config for merge ordering.
    /// Lower value = higher priority.
    /// 0 = service+env specific, 1 = service only, 2 = env only,
    /// 3 = reserved (k8s cluster), 4 = org-level (wildcard/absent)
    pub fn priority(&self) -> u8 {
        fn is_specific(s: &Option<String>) -> bool {
            s.as_deref().is_some_and(|v| v != "*")
        }
        match &self.service_target {
            None => 4,
            Some(t) => match (is_specific(&t.service), is_specific(&t.env)) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 4,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub(crate) struct TracingHeaderTag {
    pub header: String,
    pub tag_name: String,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TracingSamplingRuleProvenance {
    Customer,
    Dynamic,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct TracingSamplingRuleTag {
    pub key: String,
    pub value_glob: String,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct TracingSamplingRule {
    pub service: String,
    pub name: Option<String>,
    pub provenance: TracingSamplingRuleProvenance,
    pub resource: String,
    #[serde(default)]
    pub tags: Vec<TracingSamplingRuleTag>,
    pub sample_rate: f64,
}

/// Three-state field carrying JSON Merge Patch (RFC 7396) semantics:
///
/// - `Patch(None)` — field was absent on the wire (no change intended).
/// - `Patch(Some(None))` — field was present as JSON `null` (clear any prior remote override).
/// - `Patch(Some(Some(v)))` — field was present with a value (set to `v`).
///
/// Used by [`DynamicConfig`] for every payload field so the absent / clear /
/// set distinction survives all the way from the wire to consumers reading
/// [`Configs`]. With struct-level `#[serde(default)]` on [`DynamicConfig`],
/// missing fields fall back to [`Patch::default`] (= `Patch(None)`); present
/// fields go through this `Deserialize` impl which resolves null-vs-value via
/// the inner `Option::<T>::deserialize` and wraps the result in `Some` to
/// mark "delivered".
#[derive(Debug, Clone, PartialEq)]
pub struct Patch<T>(pub Option<Option<T>>);

impl<T> Patch<T> {
    /// `true` when the field was not delivered on the wire.
    pub fn is_absent(&self) -> bool {
        self.0.is_none()
    }
}

impl<T> Default for Patch<T> {
    fn default() -> Self {
        Patch(None)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `Deserialize::deserialize` only runs when the field WAS present on
        // the wire (absent fields go through `Default` via the struct-level
        // `#[serde(default)]`). Resolving null-vs-value via the inner
        // `Option::<T>::deserialize` lets us encode all three states.
        Option::<T>::deserialize(deserializer).map(|inner| Patch(Some(inner)))
    }
}

#[cfg(feature = "test")]
impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The struct-level `Serialize` impl for `DynamicConfig` skips absent
        // fields before reaching this method, so we only see delivered values:
        // `Some(None)` → JSON `null`, `Some(Some(v))` → `v`.
        match &self.0 {
            None => serializer.serialize_none(),
            Some(inner) => inner.serialize(serializer),
        }
    }
}

/// Dynamic configuration delivered by the APM_TRACING product.
///
/// Every field is a [`Patch<T>`] so the absent / clear / set distinction
/// survives the wire. Struct-level `#[serde(default)]` lets missing fields
/// fall back to `Patch::default()` without per-field `deserialize_with`
/// boilerplate. The matching `Serialize` impl (under `feature = "test"`) is
/// hand-written below so absent fields are omitted from the output without
/// needing per-field `skip_serializing_if` annotations. See [`Configs`] for
/// how the three states surface to consumers.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DynamicConfig {
    pub(crate) tracing_header_tags: Patch<Vec<TracingHeaderTag>>,
    pub(crate) tracing_sampling_rate: Patch<f64>,
    pub(crate) log_injection_enabled: Patch<bool>,
    pub(crate) tracing_tags: Patch<Vec<String>>,
    pub(crate) tracing_enabled: Patch<bool>,
    pub(crate) tracing_sampling_rules: Patch<Vec<TracingSamplingRule>>,
    pub(crate) dynamic_instrumentation_enabled: Patch<bool>,
    pub(crate) exception_replay_enabled: Patch<bool>,
    pub(crate) code_origin_enabled: Patch<bool>,
}

#[cfg(feature = "test")]
impl Serialize for DynamicConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Manual impl so absent fields (`Patch(None)`) are omitted from the
        // output entirely rather than serializing as JSON `null` — that
        // collapse would make round-trips re-deserialize absent fields as
        // explicit clears, defeating the three-state semantics.
        //
        // `serialize_struct`'s `len` argument is only used by formats that
        // pre-allocate (bincode, etc.); JSON ignores it. Pass the maximum.
        use serde::ser::SerializeStruct;
        macro_rules! field {
            ($s:expr, $name:ident) => {
                if !self.$name.is_absent() {
                    $s.serialize_field(stringify!($name), &self.$name)?;
                }
            };
        }
        let mut s = serializer.serialize_struct("DynamicConfig", 9)?;
        field!(s, tracing_header_tags);
        field!(s, tracing_sampling_rate);
        field!(s, log_injection_enabled);
        field!(s, tracing_tags);
        field!(s, tracing_enabled);
        field!(s, tracing_sampling_rules);
        field!(s, dynamic_instrumentation_enabled);
        field!(s, exception_replay_enabled);
        field!(s, code_origin_enabled);
        s.end()
    }
}

impl From<DynamicConfig> for Vec<Configs> {
    fn from(value: DynamicConfig) -> Self {
        // A `Configs` variant is emitted whenever the field was present on
        // the wire — including the explicit-`null` case, where the inner
        // `Option` is `None` to signal "clear". Absent fields produce no
        // variant at all, so callers can distinguish all three states.
        let mut vec = vec![];
        if let Patch(Some(tags)) = value.tracing_header_tags {
            vec.push(Configs::TracingHeaderTags(tags.map(|tags| {
                tags.into_iter().map(|t| (t.header, t.tag_name)).collect()
            })));
        }
        if let Patch(Some(sample_rate)) = value.tracing_sampling_rate {
            vec.push(Configs::TracingSamplingRate(sample_rate));
        }
        if let Patch(Some(log_injection)) = value.log_injection_enabled {
            vec.push(Configs::LogInjectionEnabled(log_injection));
        }
        if let Patch(Some(tags)) = value.tracing_tags {
            vec.push(Configs::TracingTags(tags));
        }
        if let Patch(Some(enabled)) = value.tracing_enabled {
            vec.push(Configs::TracingEnabled(enabled));
        }
        if let Patch(Some(sampling_rules)) = value.tracing_sampling_rules {
            vec.push(Configs::TracingSamplingRules(sampling_rules));
        }
        if let Patch(Some(enabled)) = value.dynamic_instrumentation_enabled {
            vec.push(Configs::DynamicInstrumentationEnabled(enabled));
        }
        if let Patch(Some(enabled)) = value.exception_replay_enabled {
            vec.push(Configs::ExceptionReplayEnabled(enabled));
        }
        if let Patch(Some(enabled)) = value.code_origin_enabled {
            vec.push(Configs::CodeOriginEnabled(enabled));
        }
        vec
    }
}

/// A single APM_TRACING field that the agent has expressed an opinion about.
///
/// Each variant carries `Option<T>`: `Some(v)` means "set this field to `v`",
/// `None` means "clear any prior remote override and fall back to local
/// config". A field that was absent on the wire produces no variant at all,
/// so callers can distinguish all three states (absent / clear / set) by
/// matching on presence-in-the-`Vec` and on the inner `Option`.
#[derive(Clone)]
pub enum Configs {
    TracingHeaderTags(Option<HashMap<String, String>>),
    TracingSamplingRate(Option<f64>),
    LogInjectionEnabled(Option<bool>),
    TracingTags(Option<Vec<String>>), // inner `Vec<String>` items are "key:val"
    TracingEnabled(Option<bool>),
    TracingSamplingRules(Option<Vec<TracingSamplingRule>>),
    DynamicInstrumentationEnabled(Option<bool>),
    ExceptionReplayEnabled(Option<bool>),
    CodeOriginEnabled(Option<bool>),
}

pub fn parse_json(data: &[u8]) -> serde_json::error::Result<DynamicConfigFile> {
    serde_json::from_slice(data)
}

#[cfg(feature = "test")]
pub mod tests {
    use super::*;

    pub fn dummy_dynamic_config(enabled: bool) -> DynamicConfigFile {
        DynamicConfigFile {
            action: "".to_string(),
            service_target: None,
            lib_config: DynamicConfig {
                tracing_enabled: Patch(Some(Some(enabled))),
                ..DynamicConfig::default()
            },
        }
    }

    #[test]
    fn parses_absent_field_as_patch_none() {
        let cfg: DynamicConfigFile = parse_json(br#"{"action": "", "lib_config": {}}"#).unwrap();
        assert!(cfg.lib_config.tracing_sampling_rate.is_absent());
        assert!(<Vec<Configs>>::from(cfg.lib_config).is_empty());
    }

    #[test]
    fn parses_explicit_null_as_clear_intent() {
        let cfg: DynamicConfigFile =
            parse_json(br#"{"action": "", "lib_config": {"tracing_sampling_rate": null}}"#)
                .unwrap();
        assert_eq!(cfg.lib_config.tracing_sampling_rate, Patch(Some(None)));
        let configs: Vec<Configs> = cfg.lib_config.into();
        assert_eq!(configs.len(), 1);
        assert!(matches!(configs[0], Configs::TracingSamplingRate(None)));
    }

    #[test]
    fn parses_concrete_value_as_set_intent() {
        let cfg: DynamicConfigFile =
            parse_json(br#"{"action": "", "lib_config": {"tracing_sampling_rate": 0.25}}"#)
                .unwrap();
        assert_eq!(
            cfg.lib_config.tracing_sampling_rate,
            Patch(Some(Some(0.25)))
        );
        let configs: Vec<Configs> = cfg.lib_config.into();
        assert_eq!(configs.len(), 1);
        assert!(matches!(configs[0], Configs::TracingSamplingRate(Some(r)) if r == 0.25));
    }

    #[test]
    fn unrelated_field_present_does_not_emit_sampling_variants() {
        // Regression guard: a payload that updates only `tracing_tags` must
        // not surface as a clear for `tracing_sampling_rate` /
        // `tracing_sampling_rules` (and vice versa). Each field is
        // independently absent / null / set.
        let cfg: DynamicConfigFile =
            parse_json(br#"{"action": "", "lib_config": {"tracing_tags": ["foo:bar"]}}"#).unwrap();
        let configs: Vec<Configs> = cfg.lib_config.into();
        assert_eq!(configs.len(), 1);
        assert!(matches!(configs[0], Configs::TracingTags(Some(_))));
        assert!(!configs
            .iter()
            .any(|c| matches!(c, Configs::TracingSamplingRate(_))));
        assert!(!configs
            .iter()
            .any(|c| matches!(c, Configs::TracingSamplingRules(_))));
    }

    #[test]
    fn round_trip_via_test_serialize_preserves_absent_vs_null() {
        // The hand-written `Serialize` impl for `DynamicConfig` must skip
        // absent fields entirely so the round-trip doesn't collapse
        // `Patch(None)` → JSON `null` → `Patch(Some(None))`.
        let mut cfg = dummy_dynamic_config(true);
        cfg.lib_config.tracing_sampling_rate = Patch(Some(None));
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: DynamicConfigFile = parse_json(json.as_bytes()).unwrap();
        // absent fields stay absent
        assert!(parsed.lib_config.tracing_sampling_rules.is_absent());
        // explicit-null stays explicit-null
        assert_eq!(parsed.lib_config.tracing_sampling_rate, Patch(Some(None)));
        // set value stays set
        assert_eq!(parsed.lib_config.tracing_enabled, Patch(Some(Some(true))));
    }
}
