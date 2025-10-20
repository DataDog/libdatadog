use std::{collections::HashMap, sync::Arc};

use serde::Serialize;

use crate::rules_based::{
    Error, EvaluationError, SdkMetadata, Str,
    error::EvaluationFailure,
    events::{AssignmentEventBase, EventMetaData},
    sharder::PreSaltedSharder,
};

use super::{
    AllocationWire, AssignmentValue, Environment, FlagWire, RuleWire, ShardRange, ShardWire,
    SplitWire, Timestamp, UniversalFlagConfigWire, VariationType,
};

#[derive(Debug)]
pub struct UniversalFlagConfig {
    /// Original JSON the configuration was compiled from.
    pub(crate) wire_json: Vec<u8>,
    pub(crate) compiled: CompiledFlagsConfig,
}

#[derive(Debug)]
pub(crate) struct CompiledFlagsConfig {
    /// When configuration was last updated.
    #[allow(dead_code)]
    pub created_at: Timestamp,
    /// Environment this configuration belongs to.
    pub environment: Environment,
    /// Flags configuration.
    ///
    /// For flags that failed to parse or are disabled, we store the evaluation failure directly.
    pub flags: HashMap<Str, Result<Flag, EvaluationFailure>>,
}

#[derive(Debug)]
pub(crate) struct Flag {
    pub variation_type: VariationType,
    pub allocations: Box<[Allocation]>,
}

#[derive(Debug)]
pub(crate) struct Allocation {
    #[allow(dead_code)]
    pub key: Str, // key is here to support evaluation details
    pub start_at: Option<Timestamp>,
    pub end_at: Option<Timestamp>,
    pub rules: Box<[RuleWire]>,
    pub splits: Box<[Split]>,
}

#[derive(Debug)]
pub(crate) struct Split {
    pub shards: Vec<Shard>,
    #[allow(dead_code)]
    pub variation_key: Str, // for evaluation details
    pub extra_logging: Arc<HashMap<String, String>>, // for evaluation details
    // This is a Result because it may still return a configuration error (invalid value for
    // assignment type).
    pub result: Result<(AssignmentValue, Option<Arc<AssignmentEventBase>>), EvaluationFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Shard {
    #[serde(skip)]
    pub(crate) sharder: PreSaltedSharder,
    pub ranges: Box<[ShardRange]>,
}

impl UniversalFlagConfig {
    pub fn from_json(meta_data: SdkMetadata, json: Vec<u8>) -> Result<Self, Error> {
        let config: UniversalFlagConfigWire = serde_json::from_slice(&json).map_err(|err| {
            log::warn!(target: "eppo", "failed to compile flag configuration: {err:?}");
            Error::EvaluationError(EvaluationError::UnexpectedConfigurationParseError)
        })?;
        Ok(UniversalFlagConfig {
            wire_json: json,
            compiled: compile_flag_configuration(meta_data.into(), config),
        })
    }

    pub fn to_json(&self) -> &[u8] {
        &self.wire_json
    }
}

fn compile_flag_configuration(
    meta_data: EventMetaData,
    config: UniversalFlagConfigWire,
) -> CompiledFlagsConfig {
    let flags = config
        .data
        .attributes
        .flags
        .into_iter()
        .map(|(key, flag)| {
            (
                key,
                Option::from(flag)
                    .ok_or(EvaluationFailure::Error(
                        EvaluationError::UnexpectedConfigurationParseError,
                    ))
                    .and_then(|flag: FlagWire| {
                        if flag.enabled {
                            Ok(compile_flag(meta_data, flag))
                        } else {
                            Err(EvaluationFailure::FlagDisabled)
                        }
                    }),
            )
        })
        .collect();

    CompiledFlagsConfig {
        created_at: config.data.attributes.created_at.into(),
        environment: config.data.attributes.environment,
        flags,
    }
}

fn compile_flag(meta_data: EventMetaData, flag: FlagWire) -> Flag {
    let variation_values = flag
        .variations
        .into_values()
        .map(|variation| {
            let assignment_value = AssignmentValue::from_wire(flag.variation_type, variation.value)
                .ok_or(EvaluationFailure::Error(
                    EvaluationError::UnexpectedConfigurationError,
                ));

            (variation.key, assignment_value)
        })
        .collect::<HashMap<_, _>>();

    let allocations = flag
        .allocations
        .into_iter()
        .map(|allocation| compile_allocation(meta_data, &flag.key, allocation, &variation_values))
        .collect();

    Flag {
        variation_type: flag.variation_type,
        allocations,
    }
}

fn compile_allocation(
    meta_data: EventMetaData,
    flag_key: &Str,
    allocation: AllocationWire,
    variation_values: &HashMap<Str, Result<AssignmentValue, EvaluationFailure>>,
) -> Allocation {
    let splits = allocation
        .splits
        .into_iter()
        .map(|split| {
            compile_split(
                meta_data,
                flag_key,
                &allocation.key,
                split,
                variation_values,
                allocation.do_log,
            )
        })
        .collect();
    Allocation {
        key: allocation.key,
        start_at: allocation.start_at,
        end_at: allocation.end_at,
        rules: allocation.rules.unwrap_or_default(),
        splits,
    }
}

fn compile_split(
    meta_data: EventMetaData,
    flag_key: &Str,
    allocation_key: &Str,
    split: SplitWire,
    variation_values: &HashMap<Str, Result<AssignmentValue, EvaluationFailure>>,
    do_log: bool,
) -> Split {
    let shards = split
        .shards
        .into_iter()
        // `compile_shard` may return `None` for shards that are
        // "insignificant", meaning that they *always* match, so they don't even
        // need to be checked. We filter out such shards here with
        // `.filter_map()`.
        .filter_map(compile_shard)
        .collect();

    let extra_logging = split.extra_logging.unwrap_or_default();

    let result = variation_values
        .get(&split.variation_key)
        .cloned()
        .unwrap_or(Err(EvaluationFailure::Error(
            EvaluationError::UnexpectedConfigurationError,
        )))
        .map(|value| {
            let event = do_log.then(|| {
                Arc::new(AssignmentEventBase {
                    experiment: format!("{flag_key}-{allocation_key}"),
                    feature_flag: flag_key.clone(),
                    allocation: allocation_key.clone(),
                    variation: split.variation_key.clone(),
                    meta_data,
                    extra_logging: extra_logging.clone(),
                })
            });
            (value, event)
        });

    Split {
        shards,
        variation_key: split.variation_key,
        extra_logging,
        result,
    }
}

fn compile_shard(shard: ShardWire) -> Option<Shard> {
    if shard.ranges.contains(&ShardRange {
        start: 0,
        end: shard.total_shards,
    }) {
        // The shard is "insignificant" because it always matches, so we don't need to waste time
        // checking it.
        None
    } else {
        Some(Shard {
            sharder: PreSaltedSharder::new(&[shard.salt.as_bytes(), b"-"], shard.total_shards),
            ranges: shard.ranges,
        })
    }
}
