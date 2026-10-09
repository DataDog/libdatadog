// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Projection of observations into metric points. Each projector makes the
//! checks of `PORTABLE-METRICS.md#order-of-checks` in the contract's order
//! and returns the first failing code.

use crate::number::{
    CountReading, integer_to_f64, nano_usd_from_number, nano_usd_from_usd, read_count,
    usd_from_nano_usd,
};
use crate::observation::{
    GatewayRequestObservation, OBSERVATION_POINTS, ProviderAttemptObservation,
    ProviderStreamingObservation, TOKEN_SOURCES, TokenBreakdownObservation, is_context_band,
    is_identifier, is_rate_card,
};
use crate::point::{
    Attributes, CounterSet, ErrorCode, IssueCode, Issues, MetricError, MetricPoint, Projection,
    reject,
};
use crate::usage::{Modality, NormalizedUsage, normalize_usage};

const TOKEN_MODALITY: &str = "gen_ai.token.modality";
const CACHE_LIFETIME: &str = "trajectory.cache.lifetime";
const CONTEXT_BAND: &str = "trajectory.context.band";
const SERVICE_TIER: &str = "trajectory.request.service_tier";
const BATCH: &str = "trajectory.request.batch";
const OBSERVATION_POINT: &str = "trajectory.observation.point";
const TOKEN_SOURCE: &str = "trajectory.token.source";
const TOKEN: &str = "{token}";

const USAGE_OPERATIONS: [&str; 3] = ["chat", "text_completion", "embeddings"];
const CHAT_OPERATIONS: [&str; 2] = ["chat", "text_completion"];

fn with(base: &Attributes, key: &str, value: &str) -> Attributes {
    let mut attributes = base.clone();
    attributes.insert(key.into(), value.into());
    attributes
}

fn with_some(base: &Attributes, key: &str, value: Option<&str>) -> Attributes {
    match value {
        Some(value) => with(base, key, value),
        None => base.clone(),
    }
}

/// What a token or cost Counter carries beyond its base attributes: the
/// context band, the service tier and the batch flag, each when the
/// observation states it.
#[derive(Clone, Copy)]
struct PriceTags<'a> {
    band: Option<&'a str>,
    tier: Option<&'a str>,
    batch: bool,
}

/// The batch attribute has one value, `true`: a false batch adds nothing.
fn with_price_tags(base: &Attributes, tags: PriceTags) -> Attributes {
    let mut out = with_some(
        &with_some(base, CONTEXT_BAND, tags.band),
        SERVICE_TIER,
        tags.tier,
    );
    if tags.batch {
        out.insert(BATCH.into(), "true".into());
    }
    out
}

fn count_value(count: u64) -> f64 {
    integer_to_f64(u128::from(count))
}

/// Step 4: the operation is required and must be one the profile covers.
fn check_operation<'a>(name: &'a Option<String>, allowed: &[&str]) -> Result<&'a str, MetricError> {
    match name.as_deref() {
        None => reject(
            ErrorCode::RequiredFieldMissing,
            "operation_name is required",
        ),
        Some(name) if allowed.contains(&name) => Ok(name),
        Some(_) => reject(
            ErrorCode::ValueNotAllowed,
            format!("operation_name must be one of {}", allowed.join(", ")),
        ),
    }
}

/// Step 5: the provider is required and must be an identifier.
fn check_provider(name: &Option<String>) -> Result<&str, MetricError> {
    match name.as_deref() {
        None => reject(ErrorCode::RequiredFieldMissing, "provider_name is required"),
        Some(name) if is_identifier(name) => Ok(name),
        Some(_) => reject(
            ErrorCode::IdentifierInvalid,
            "provider_name must be a low-cardinality identifier",
        ),
    }
}

/// A duration is a finite, non-negative number. An infinity stands for a
/// number no double holds, which the contract rejects the same way.
fn is_duration(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn check_duration(value: Option<f64>, name: &str) -> Result<f64, MetricError> {
    match value {
        Some(value) if is_duration(value) => Ok(value),
        _ => reject(
            ErrorCode::DurationInvalid,
            format!("{name} must be a non-negative number"),
        ),
    }
}

fn check_context_band(band: &Option<String>) -> Result<Option<&str>, MetricError> {
    match band.as_deref() {
        Some(band) if !is_context_band(band) => reject(
            ErrorCode::IdentifierInvalid,
            "context_band must be 1 to 32 letters, digits, '.', '_' or '-'",
        ),
        band => Ok(band),
    }
}

/// The tier is the provider's own word, in the grammar of a context band.
fn check_service_tier(tier: &Option<String>) -> Result<Option<&str>, MetricError> {
    match tier.as_deref() {
        Some(tier) if !is_context_band(tier) => reject(
            ErrorCode::IdentifierInvalid,
            "service_tier must be 1 to 32 letters, digits, '.', '_' or '-'",
        ),
        tier => Ok(tier),
    }
}

/// The model's canonical name (`PORTABLE-METRICS.md#canonical-model`): the
/// provider is an identifier, no stated part is empty, and the other parts
/// are stated only beside the canonical model.
fn check_canonical_name(o: &ProviderAttemptObservation) -> Result<(), MetricError> {
    if o.canonical_provider
        .as_deref()
        .is_some_and(|provider| !is_identifier(provider))
    {
        return reject(
            ErrorCode::IdentifierInvalid,
            "canonical_provider must be a low-cardinality identifier",
        );
    }
    let parts = [
        ("canonical_model", &o.canonical_model),
        ("model_family", &o.model_family),
        ("cost_priced_as_model", &o.cost_priced_as_model),
    ];
    for (name, value) in parts {
        if value.as_deref() == Some("") {
            return Err(MetricError::new(
                ErrorCode::ValueNotAllowed,
                format!("{name} must not be empty"),
            ));
        }
    }
    if o.canonical_model.is_none() {
        let beside = [
            ("canonical_provider", &o.canonical_provider),
            ("model_family", &o.model_family),
            ("cost_priced_as_model", &o.cost_priced_as_model),
        ];
        for (name, value) in beside {
            if value.is_some() {
                return Err(MetricError::new(
                    ErrorCode::FieldsContradict,
                    format!("{name} requires canonical_model"),
                ));
            }
        }
    }
    Ok(())
}

fn check_error_type(error_type: &Option<String>) -> Result<Option<&str>, MetricError> {
    match error_type.as_deref() {
        Some(value) if !is_identifier(value) => reject(
            ErrorCode::IdentifierInvalid,
            "error_type must be a low-cardinality identifier",
        ),
        value => Ok(value),
    }
}

/// The required `observation_point` of the provider profiles
/// (`PORTABLE-METRICS.md#observation-point`). Absent or `null` is rejected:
/// the profile does not guess where the operation was observed.
fn check_observation_point(point: &Option<String>) -> Result<Option<&str>, MetricError> {
    match point.as_deref() {
        None => reject(
            ErrorCode::RequiredFieldMissing,
            "observation_point is required",
        ),
        Some(point) if !OBSERVATION_POINTS.contains(&point) => reject(
            ErrorCode::ValueNotAllowed,
            format!(
                "observation_point must be one of {}",
                OBSERVATION_POINTS.join(", ")
            ),
        ),
        point => Ok(point),
    }
}

/// The two token sources, checked after the bases and modality keys
/// (`PORTABLE-METRICS.md#token-source`). Returns whether the input and the
/// output side were estimated.
fn check_token_sources(
    input: &Option<String>,
    output: &Option<String>,
) -> Result<(bool, bool), MetricError> {
    for (name, value) in [
        ("input_token_source", input),
        ("output_token_source", output),
    ] {
        if let Some(value) = value.as_deref() {
            if !TOKEN_SOURCES.contains(&value) {
                return reject(
                    ErrorCode::ValueNotAllowed,
                    format!("{name} must be reported or estimated"),
                );
            }
        }
    }
    Ok((estimated_side(input), estimated_side(output)))
}

fn estimated_side(source: &Option<String>) -> bool {
    source.as_deref() == Some("estimated")
}

/// The attributes of a token point, marked when its side was estimated.
fn with_token_source(base: &Attributes, estimated: bool) -> Attributes {
    if estimated {
        with(base, TOKEN_SOURCE, "estimated")
    } else {
        base.clone()
    }
}

fn provider_attributes(
    operation_name: &str,
    provider_name: &str,
    request_model: &Option<String>,
    response_model: &Option<String>,
    observation_point: Option<&str>,
) -> Attributes {
    let mut attributes = Attributes::new();
    attributes.insert("gen_ai.operation.name".into(), operation_name.into());
    attributes.insert("gen_ai.provider.name".into(), provider_name.into());
    if let Some(model) = request_model {
        attributes.insert("gen_ai.request.model".into(), model.clone());
    }
    if let Some(model) = response_model {
        attributes.insert("gen_ai.response.model".into(), model.clone());
    }
    // Every point says where the operation was observed, when stated.
    if let Some(point) = observation_point {
        attributes.insert(OBSERVATION_POINT.into(), point.into());
    }
    attributes
}

/// The cost of one observation after the contradiction checks of step 9.
struct Cost<'a> {
    source: &'a str,
    /// `None` when the amount is unusable.
    amount: Option<(u64, f64)>,
}

/// Step 9: the contradictory-cost rules, in the contract's order. Returns
/// `None` when the observation states no cost.
fn check_cost(o: &ProviderAttemptObservation) -> Result<Option<Cost<'_>>, MetricError> {
    let has_cost = o.cost_usd.is_some() || o.cost_nano_usd.is_some();
    let source = o.cost_source.as_deref();
    if has_cost && source.is_none() {
        return reject(
            ErrorCode::FieldsContradict,
            "cost_source is required when a cost is present",
        );
    }
    if source.is_some() && !has_cost {
        return reject(
            ErrorCode::FieldsContradict,
            "a cost is required when cost_source is present",
        );
    }
    if source.is_some_and(|s| !matches!(s, "reported" | "calculated" | "estimated")) {
        return reject(
            ErrorCode::ValueNotAllowed,
            "cost_source must be reported, calculated, or estimated",
        );
    }
    if o.cost_bound
        .as_deref()
        .is_some_and(|bound| bound != "lower")
    {
        return reject(ErrorCode::ValueNotAllowed, "cost_bound must be lower");
    }
    let calculated = source == Some("calculated");
    // A bound says a card's prices were applied to less than what was used: a
    // calculation, or an estimate that names its card.
    let estimated_with_card = source == Some("estimated") && o.cost_rate_card.is_some();
    if o.cost_bound.is_some() && !calculated && !estimated_with_card {
        return reject(
            ErrorCode::FieldsContradict,
            "cost_bound requires cost_source calculated, or estimated with cost_rate_card",
        );
    }
    // A calculated cost has a card and an estimate may name one; a reported
    // cost has none.
    if o.cost_rate_card.is_some() && matches!(source, None | Some("reported")) {
        return reject(
            ErrorCode::FieldsContradict,
            "cost_rate_card is only valid for a calculated or estimated cost",
        );
    }
    let Some(source) = source else {
        // A priced-as model with no cost at all.
        if o.cost_priced_as_model.is_some() {
            return reject(
                ErrorCode::FieldsContradict,
                "cost_priced_as_model requires cost_source estimated",
            );
        }
        return Ok(None);
    };
    let from_nano = o.cost_nano_usd.map(nano_usd_from_number);
    let from_usd = o.cost_usd.map(nano_usd_from_usd);
    if let (Some(Some(nano)), Some(Some(usd))) = (from_nano, from_usd) {
        if nano != usd {
            return reject(
                ErrorCode::FieldsContradict,
                "cost_usd and cost_nano_usd state different amounts",
            );
        }
    }
    // A calculated cost is arithmetic over reported usage, which an
    // estimated side is not.
    if source == "calculated"
        && (estimated_side(&o.input_token_source) || estimated_side(&o.output_token_source))
    {
        return reject(
            ErrorCode::FieldsContradict,
            "cost_source calculated is not valid beside an estimated token side",
        );
    }
    // A fallback changes which model is priced, so its cost is an estimate.
    if o.cost_priced_as_model.is_some() && source != "estimated" {
        return reject(
            ErrorCode::FieldsContradict,
            "cost_priced_as_model requires cost_source estimated",
        );
    }
    // One unusable stated amount makes the whole cost unusable.
    let amount = match (from_nano, from_usd, o.cost_usd) {
        (Some(None), _, _) | (_, Some(None), _) => None,
        (Some(Some(nano)), _, usd) => Some((nano, usd.unwrap_or_else(|| usd_from_nano_usd(nano)))),
        (None, Some(Some(nano)), Some(usd)) => Some((nano, usd)),
        _ => None,
    };
    Ok(Some(Cost { source, amount }))
}

/// The issues the provider-attempt profile reports from normalization.
/// The duration metric of a provider operation. The upstream conventions
/// record an inference operation (chat, text_completion) under
/// `gen_ai.client.inference.duration` and every other operation, such as
/// embeddings, under `gen_ai.client.operation.duration`.
fn duration_metric(operation: &str) -> &'static str {
    if operation == "embeddings" {
        "gen_ai.client.operation.duration"
    } else {
        "gen_ai.client.inference.duration"
    }
}

fn provider_attempt_issues(usage: &NormalizedUsage) -> Issues {
    usage
        .issues
        .iter()
        .copied()
        .filter(|issue| match issue {
            IssueCode::InputBasisMissing
            | IssueCode::OutputBasisMissing
            | IssueCode::InputTotalMissing
            | IssueCode::OutputTotalMissing
            | IssueCode::UsageInconsistent
            | IssueCode::CountOutOfRange
            | IssueCode::ServerToolCountConflict => true,
            IssueCode::CacheReadMissing | IssueCode::CacheWriteMissing => {
                usage.missing_cache_part_prevents_total
            }
            _ => false,
        })
        .collect()
}

/// The issues the token-breakdown profile reports from normalization.
fn token_breakdown_issues(usage: &NormalizedUsage) -> Issues {
    usage
        .issues
        .iter()
        .copied()
        .filter(|issue| {
            matches!(
                issue,
                IssueCode::InputBasisMissing
                    | IssueCode::OutputBasisMissing
                    | IssueCode::CacheReadMissing
                    | IssueCode::CacheWriteMissing
                    | IssueCode::CacheWriteLifetimeUnknown
                    | IssueCode::UsageInconsistent
                    | IssueCode::CountOutOfRange
            )
        })
        .collect()
}

fn token_points(
    points: &mut Vec<MetricPoint>,
    direction: &str,
    total: Option<u64>,
    by_modality: &[(Modality, u64)],
    base: &Attributes,
    tags: PriceTags,
    estimated: bool,
) {
    let base = &with_token_source(base, estimated);
    if let Some(total) = total {
        points.push(MetricPoint::histogram(
            &format!("gen_ai.client.inference.operation.{direction}_tokens"),
            TOKEN,
            count_value(total),
            base.clone(),
        ));
    }
    let counter = with_price_tags(base, tags);
    for (modality, count) in by_modality {
        points.push(MetricPoint::counter(
            &format!("gen_ai.client.inference.usage.{direction}_tokens"),
            TOKEN,
            count_value(*count),
            with(&counter, TOKEN_MODALITY, modality.as_str()),
        ));
    }
}

/// Project one provider operation (`gen_ai.client.provider_attempt@0.1.0`).
pub fn project_provider_attempt(o: &ProviderAttemptObservation) -> Result<Projection, MetricError> {
    o.usage.check_counts()?;
    o.usage.check_values()?;
    let (input_estimated, output_estimated) =
        check_token_sources(&o.input_token_source, &o.output_token_source)?;
    let operation = check_operation(&o.operation_name, &USAGE_OPERATIONS)?;
    let provider = check_provider(&o.provider_name)?;
    let duration = check_duration(o.duration_seconds, "duration_seconds")?;
    let Some(streaming) = o.streaming else {
        return reject(ErrorCode::RequiredFieldMissing, "streaming is required");
    };
    let first_chunk = match o.time_to_first_chunk_seconds {
        None => None,
        Some(_) => {
            let seconds =
                check_duration(o.time_to_first_chunk_seconds, "time_to_first_chunk_seconds")?;
            if !streaming {
                return reject(
                    ErrorCode::FieldsContradict,
                    "time_to_first_chunk_seconds is only valid for streaming operations",
                );
            }
            Some(seconds)
        }
    };
    let tags = PriceTags {
        band: check_context_band(&o.context_band)?,
        tier: check_service_tier(&o.service_tier)?,
        batch: o.batch == Some(true),
    };
    check_canonical_name(o)?;
    let cost = check_cost(o)?;
    let error_type = check_error_type(&o.error_type)?;
    let observation_point = check_observation_point(&o.observation_point)?;

    let usage = normalize_usage(&o.usage, operation == "embeddings")?;
    let mut issues = provider_attempt_issues(&usage);
    let base = provider_attributes(
        operation,
        provider,
        &o.request_model,
        &o.response_model,
        observation_point,
    );

    let mut points = vec![MetricPoint::histogram(
        duration_metric(operation),
        "s",
        duration,
        with_some(&base, "error.type", error_type),
    )];
    token_points(
        &mut points,
        "input",
        usage.input_total,
        &usage.input_by_modality,
        &base,
        tags,
        input_estimated,
    );
    token_points(
        &mut points,
        "output",
        usage.output_total,
        &usage.output_by_modality,
        &base,
        tags,
        output_estimated,
    );
    if let Some(seconds) = first_chunk {
        points.push(MetricPoint::histogram(
            "gen_ai.client.inference.time_to_first_chunk",
            "s",
            seconds,
            base.clone(),
        ));
    }
    if let Some(requests) = usage.web_search_requests {
        points.push(MetricPoint::counter(
            "trajectory.gen_ai.client.inference.usage.server_tool.requests",
            "{request}",
            count_value(requests),
            with(&base, "trajectory.server_tool.kind", "web_search"),
        ));
    }
    if let Some(cost) = cost {
        match cost.amount {
            None => {
                issues.insert(IssueCode::CostUnusable);
            }
            Some((nano_usd, usd)) => {
                // A card outside the grammar drops the label of a cost that
                // is recorded; the cost itself is kept.
                let rate_card = match o.cost_rate_card.as_deref() {
                    Some(card) if !is_rate_card(card) => {
                        issues.insert(IssueCode::CostRateCardDropped);
                        None
                    }
                    card => card,
                };
                // Both cost points carry the canonical name beside the names
                // as received; no other point does, and nothing is filled in.
                let mut sourced = with(&base, "trajectory.cost.source", cost.source);
                for (key, value) in [
                    ("trajectory.canonical.provider", &o.canonical_provider),
                    ("trajectory.canonical.model", &o.canonical_model),
                    ("trajectory.canonical.family", &o.model_family),
                ] {
                    if let Some(value) = value {
                        sourced.insert(key.into(), value.clone());
                    }
                }
                let mut counter = with_some(&sourced, "trajectory.cost.rate_card", rate_card);
                if let Some(model) = o.cost_priced_as_model.as_deref() {
                    counter.insert("trajectory.cost.priced_as".into(), model.into());
                }
                if let Some(bound) = o.cost_bound.as_deref() {
                    counter.insert("trajectory.cost.bound".into(), bound.into());
                }
                points.push(MetricPoint::counter(
                    "trajectory.gen_ai.client.inference.usage.cost",
                    "{nanoUSD}",
                    count_value(nano_usd),
                    with_price_tags(&counter, tags),
                ));
                points.push(MetricPoint::histogram(
                    "trajectory.gen_ai.client.operation.cost",
                    "{USD}",
                    usd,
                    sourced,
                ));
            }
        }
    }
    Ok(Projection {
        points,
        issues,
        ..Projection::default()
    })
}

/// Project the chunk intervals of one streamed operation
/// (`gen_ai.client.provider_streaming@0.1.0`).
pub fn project_provider_streaming(
    o: &ProviderStreamingObservation,
) -> Result<Projection, MetricError> {
    let operation = check_operation(&o.operation_name, &CHAT_OPERATIONS)?;
    let provider = check_provider(&o.provider_name)?;
    match o.streaming {
        None => return reject(ErrorCode::RequiredFieldMissing, "streaming is required"),
        Some(false) => return reject(ErrorCode::FieldsContradict, "streaming must be true"),
        Some(true) => {}
    }
    let Some(intervals) = &o.output_chunk_intervals_seconds else {
        return reject(
            ErrorCode::RequiredFieldMissing,
            "output_chunk_intervals_seconds is required",
        );
    };
    if !intervals.iter().all(|interval| is_duration(*interval)) {
        return reject(
            ErrorCode::DurationInvalid,
            "output_chunk_intervals_seconds must contain non-negative numbers",
        );
    }
    let observation_point = check_observation_point(&o.observation_point)?;
    let base = provider_attributes(
        operation,
        provider,
        &o.request_model,
        &o.response_model,
        observation_point,
    );
    Ok(Projection {
        points: intervals
            .iter()
            .map(|interval| {
                MetricPoint::histogram(
                    "gen_ai.client.inference.time_per_output_chunk",
                    "s",
                    *interval,
                    base.clone(),
                )
            })
            .collect(),
        ..Projection::default()
    })
}

/// Project the cache and reasoning parts of one provider operation
/// (`trajectory.gen_ai.client.token_breakdown@0.1.0`).
pub fn project_token_breakdown(o: &TokenBreakdownObservation) -> Result<Projection, MetricError> {
    o.usage.check_counts()?;
    o.usage.check_values()?;
    let (input_estimated, output_estimated) =
        check_token_sources(&o.input_token_source, &o.output_token_source)?;
    let operation = check_operation(&o.operation_name, &USAGE_OPERATIONS)?;
    let provider = check_provider(&o.provider_name)?;
    let embeddings = operation == "embeddings";
    // A total above the bound is present: only an absent one is rejected.
    if o.usage.input_tokens.is_none() {
        return reject(ErrorCode::RequiredFieldMissing, "input_tokens is required");
    }
    if o.usage.output_tokens.is_none() && !embeddings {
        return reject(ErrorCode::RequiredFieldMissing, "output_tokens is required");
    }
    let tags = PriceTags {
        band: check_context_band(&o.context_band)?,
        tier: check_service_tier(&o.service_tier)?,
        batch: o.batch == Some(true),
    };
    let observation_point = check_observation_point(&o.observation_point)?;

    let usage = normalize_usage(&o.usage, embeddings)?;
    let base = provider_attributes(
        operation,
        provider,
        &o.request_model,
        &o.response_model,
        observation_point,
    );
    let base = with_price_tags(
        &with(&base, TOKEN_MODALITY, Modality::Unknown.as_str()),
        tags,
    );
    let input_side = with_token_source(&base, input_estimated);
    let counter = |name: &str, count: u64, attributes: Attributes| {
        MetricPoint::counter(name, TOKEN, count_value(count), attributes)
    };
    let mut points = Vec::new();
    if let Some(read) = usage.cache_read {
        points.push(counter(
            "gen_ai.client.inference.usage.cache_read.input_tokens",
            read,
            input_side.clone(),
        ));
    }
    for (lifetime, count) in [
        ("5m", usage.cache_write_5m),
        ("1h", usage.cache_write_1h),
        ("unspecified", usage.cache_write_unspecified),
    ] {
        if let Some(count) = count {
            points.push(counter(
                "gen_ai.client.inference.usage.cache_write.input_tokens",
                count,
                with(&input_side, CACHE_LIFETIME, lifetime),
            ));
        }
    }
    if let Some(uncached) = usage.uncached_input {
        points.push(counter(
            "trajectory.gen_ai.client.inference.usage.uncached.input_tokens",
            uncached,
            input_side.clone(),
        ));
    }
    if let Some(reasoning) = usage.reasoning {
        points.push(counter(
            "gen_ai.client.inference.usage.reasoning.output_tokens",
            reasoning,
            with_token_source(&base, output_estimated),
        ));
    }
    Ok(Projection {
        points,
        issues: token_breakdown_issues(&usage),
        ..Projection::default()
    })
}

/// A count of a profile that has no issue codes: negative, fractional, or
/// too large for a double is `count_invalid`.
fn check_plain_count(value: Option<f64>, name: &str) -> Result<Option<f64>, MetricError> {
    match value {
        None => Ok(None),
        Some(value) if value.is_finite() && read_count(value) != CountReading::Malformed => {
            Ok(Some(value))
        }
        Some(_) => reject(
            ErrorCode::CountInvalid,
            format!("{name} must be a non-negative integer"),
        ),
    }
}

/// Project one logical gateway request
/// (`trajectory.gen_ai.gateway.request@0.1.0`).
pub fn project_gateway_request(o: &GatewayRequestObservation) -> Result<Projection, MetricError> {
    // The operations the provider profiles cover (`GATEWAY-METRICS.md#operations`).
    let operation = check_operation(&o.operation_name, &USAGE_OPERATIONS)?;
    let duration = check_duration(o.duration_seconds, "duration_seconds")?;
    let operations = check_plain_count(o.provider_operations, "provider_operations")?;
    let retries = check_plain_count(o.retries, "retries")?;
    let fallbacks = check_plain_count(o.fallbacks, "fallbacks")?;
    let coverage = o.provider_operation_coverage.as_deref();
    if operations.is_some() && coverage.is_none() {
        return reject(
            ErrorCode::FieldsContradict,
            "provider_operation_coverage is required when provider_operations is present",
        );
    }
    if coverage.is_some() && operations.is_none() {
        return reject(
            ErrorCode::FieldsContradict,
            "provider_operations is required when provider_operation_coverage is present",
        );
    }
    if coverage.is_some_and(|c| !matches!(c, "complete" | "partial")) {
        return reject(
            ErrorCode::ValueNotAllowed,
            "provider_operation_coverage must be complete or partial",
        );
    }
    let outcomes = o.cache_outcomes.as_deref().unwrap_or_default();
    if outcomes
        .iter()
        .any(|outcome| !matches!(outcome.as_str(), "hit" | "miss" | "write"))
    {
        return reject(
            ErrorCode::ValueNotAllowed,
            "cache_outcomes must contain hit, miss, or write",
        );
    }
    let estimated_from_nano = o.estimated_cost_nano_usd.map(nano_usd_from_number);
    let estimated_from_usd = o.estimated_cost_usd.map(nano_usd_from_usd);
    if let (Some(Some(nano)), Some(Some(usd))) = (estimated_from_nano, estimated_from_usd) {
        if nano != usd {
            return reject(
                ErrorCode::FieldsContradict,
                "estimated_cost_usd and estimated_cost_nano_usd disagree",
            );
        }
    }
    let error_type = check_error_type(&o.error_type)?;
    // Only a gateway observes a logical gateway request.
    if o.observation_point
        .as_deref()
        .is_some_and(|p| p != "gateway")
    {
        return reject(
            ErrorCode::ValueNotAllowed,
            "observation_point must be gateway in the gateway-request profile",
        );
    }

    // Every point says it was observed at a gateway, stated or not.
    let mut base = Attributes::new();
    base.insert("gen_ai.operation.name".into(), operation.into());
    base.insert(OBSERVATION_POINT.into(), "gateway".into());
    if let Some(model) = &o.request_model {
        base.insert("gen_ai.request.model".into(), model.clone());
    }
    let mut points = vec![MetricPoint::histogram(
        "trajectory.gen_ai.gateway.request.duration",
        "s",
        duration,
        with_some(&base, "error.type", error_type),
    )];
    if let (Some(count), Some(coverage)) = (operations, coverage) {
        points.push(MetricPoint::histogram(
            "trajectory.gen_ai.gateway.request.provider_operations",
            "{operation}",
            count,
            with(&base, "trajectory.provider.operation.coverage", coverage),
        ));
    }
    for (name, unit, value) in [
        (
            "trajectory.gen_ai.gateway.request.retries",
            "{retry}",
            retries,
        ),
        (
            "trajectory.gen_ai.gateway.request.fallbacks",
            "{fallback}",
            fallbacks,
        ),
    ] {
        if let Some(value) = value {
            points.push(MetricPoint::histogram(name, unit, value, base.clone()));
        }
    }
    let mut cache = CounterSet::default();
    for outcome in outcomes {
        cache.add_count(
            "trajectory.gen_ai.gateway.cache.operations",
            "{operation}",
            1,
            with(&base, "trajectory.cache.outcome", outcome),
        );
    }
    points.extend(cache.into_points());
    // The gateway's own figure for the request. It names no source: the
    // metric's name says it is an estimate. One unusable stated amount makes
    // the figure unusable.
    let mut issues = Issues::default();
    match (estimated_from_nano, estimated_from_usd) {
        (None, None) => {}
        (Some(None), _) | (_, Some(None)) => {
            issues.insert(IssueCode::CostUnusable);
        }
        (Some(Some(nano)), _) | (None, Some(Some(nano))) => {
            points.push(MetricPoint::counter(
                "trajectory.gen_ai.gateway.request.estimated_cost",
                "{nanoUSD}",
                count_value(nano),
                base.clone(),
            ));
        }
    }
    Ok(Projection {
        points,
        issues,
        ..Projection::default()
    })
}
