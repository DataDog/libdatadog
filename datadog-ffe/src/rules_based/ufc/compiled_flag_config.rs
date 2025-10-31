// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, sync::Arc};

use serde::Deserialize;

use crate::rules_based::{
    error::EvaluationFailure, sharder::PreSaltedSharder, EvaluationError, Str, Timestamp,
};

use super::{
    AllocationWire, AssignmentValue, Environment, FlagWire, RuleWire, ShardRange, ShardWire,
    SplitWire, UniversalFlagConfigWire, VariationType,
};

#[derive(Debug)]
pub struct UniversalFlagConfig {
    /// Original JSON the configuration was compiled from.
    pub(crate) wire_json: Vec<u8>,
    pub(crate) compiled: CompiledFlagsConfig,
}

#[derive(Debug, Deserialize)]
#[serde(from = "UniversalFlagConfigWire")]
pub(crate) struct CompiledFlagsConfig {
    /// When configuration was last updated.
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
    pub key: Str,
    pub start_at: Option<Timestamp>,
    pub end_at: Option<Timestamp>,
    pub rules: Box<[RuleWire]>,
    pub splits: Box<[Split]>,
    pub do_log: bool,
}

#[derive(Debug)]
pub(crate) struct Split {
    pub shards: Vec<Shard>,
    pub variation_key: Str,
    pub extra_logging: Arc<HashMap<String, String>>,
    pub value: AssignmentValue,
}

#[derive(Debug, Clone)]
pub(crate) struct Shard {
    pub sharder: PreSaltedSharder,
    pub ranges: Box<[ShardRange]>,
}

impl UniversalFlagConfig {
    pub fn from_json(json: Vec<u8>) -> Result<Self, serde_json::Error> {
        let config: CompiledFlagsConfig = serde_json::from_slice(&json)?;
        Ok(UniversalFlagConfig {
            wire_json: json,
            compiled: config,
        })
    }

    pub fn to_json(&self) -> &[u8] {
        &self.wire_json
    }
}

impl From<UniversalFlagConfigWire> for CompiledFlagsConfig {
    fn from(config: UniversalFlagConfigWire) -> Self {
        let flags = config
            .flags
            .into_iter()
            .map(|(key, flag)| {
                (
                    key,
                    Option::from(flag)
                        .ok_or(EvaluationFailure::Error(
                            EvaluationError::UnexpectedConfigurationError,
                        ))
                        .and_then(compile_flag),
                )
            })
            .collect();

        CompiledFlagsConfig {
            created_at: config.created_at,
            environment: config.environment,
            flags,
        }
    }
}

fn compile_flag(flag: FlagWire) -> Result<Flag, EvaluationFailure> {
    if !flag.enabled {
        return Err(EvaluationFailure::FlagDisabled);
    }

    let variation_values = flag
        .variations
        .into_values()
        .map(|variation| {
            let assignment_value = AssignmentValue::from_wire(flag.variation_type, variation.value)
                .ok_or(EvaluationError::UnexpectedConfigurationError)?;

            Ok((variation.key, assignment_value))
        })
        .collect::<Result<HashMap<_, _>, EvaluationError>>()?;

    let allocations = flag
        .allocations
        .into_iter()
        .map(|allocation| compile_allocation(allocation, &variation_values))
        .collect::<Result<_, _>>()?;

    Ok(Flag {
        variation_type: flag.variation_type,
        allocations,
    })
}

fn compile_allocation(
    allocation: AllocationWire,
    variation_values: &HashMap<Str, AssignmentValue>,
) -> Result<Allocation, EvaluationError> {
    let splits = allocation
        .splits
        .into_iter()
        .map(|split| compile_split(split, variation_values))
        .collect::<Result<_, _>>()?;
    Ok(Allocation {
        key: allocation.key,
        start_at: allocation.start_at,
        end_at: allocation.end_at,
        rules: allocation.rules.unwrap_or_default(),
        splits,
        do_log: allocation.do_log,
    })
}

fn compile_split(
    split: SplitWire,
    variation_values: &HashMap<Str, AssignmentValue>,
) -> Result<Split, EvaluationError> {
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
        .ok_or(EvaluationError::UnexpectedConfigurationError)?;

    Ok(Split {
        shards,
        variation_key: split.variation_key,
        extra_logging,
        value: result,
    })
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
