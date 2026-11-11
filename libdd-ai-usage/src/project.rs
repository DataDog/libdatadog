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
    Attrs, ErrorCode, IssueCode, Issues, MetricError, MetricPoint, Projection, Sink, reject,
};
use crate::usage::{Modality, NormalizedUsage, normalize_provider_usage};

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
fn with_price_tags<'a>(base: Attrs<'a>, tags: PriceTags<'a>) -> Attrs<'a> {
    base.with_some(CONTEXT_BAND, tags.band)
        .with_some(SERVICE_TIER, tags.tier)
        .with_some(BATCH, tags.batch.then_some("true"))
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
        if let Some(value) = value.as_deref()
            && !TOKEN_SOURCES.contains(&value)
        {
            return reject(
                ErrorCode::ValueNotAllowed,
                format!("{name} must be reported or estimated"),
            );
        }
    }
    Ok((estimated_side(input), estimated_side(output)))
}

fn estimated_side(source: &Option<String>) -> bool {
    source.as_deref() == Some("estimated")
}

/// The attributes of a token point, marked when its side was estimated.
fn with_token_source(base: Attrs<'_>, estimated: bool) -> Attrs<'_> {
    base.with_some(TOKEN_SOURCE, estimated.then_some("estimated"))
}

fn provider_attributes<'a>(
    operation_name: &'a str,
    provider_name: &'a str,
    request_model: &'a Option<String>,
    response_model: &'a Option<String>,
    observation_point: Option<&'a str>,
) -> Attrs<'a> {
    // Every point says where the operation was observed, when stated.
    Attrs::new()
        .with("gen_ai.operation.name", operation_name)
        .with("gen_ai.provider.name", provider_name)
        .with_some("gen_ai.request.model", request_model.as_deref())
        .with_some("gen_ai.response.model", response_model.as_deref())
        .with_some(OBSERVATION_POINT, observation_point)
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
    if let (Some(Some(nano)), Some(Some(usd))) = (from_nano, from_usd)
        && nano != usd
    {
        return reject(
            ErrorCode::FieldsContradict,
            "cost_usd and cost_nano_usd state different amounts",
        );
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

/// The two metrics of one token direction: the per-operation Histogram and
/// the usage Counter.
struct Direction {
    operation: &'static str,
    usage: &'static str,
}

const INPUT: Direction = Direction {
    operation: "gen_ai.client.inference.operation.input_tokens",
    usage: "gen_ai.client.inference.usage.input_tokens",
};
const OUTPUT: Direction = Direction {
    operation: "gen_ai.client.inference.operation.output_tokens",
    usage: "gen_ai.client.inference.usage.output_tokens",
};

#[allow(clippy::too_many_arguments)]
fn token_points<'a>(
    sink: &mut impl Sink<'a>,
    direction: &Direction,
    total: Option<u64>,
    by_modality: &[(Modality, u64)],
    base: Attrs<'a>,
    tags: PriceTags<'a>,
    estimated: bool,
) {
    let base = with_token_source(base, estimated);
    if let Some(total) = total {
        sink.histogram(direction.operation, TOKEN, count_value(total), &base);
    }
    let counter = with_price_tags(base, tags);
    for (modality, count) in by_modality {
        sink.counter(
            direction.usage,
            TOKEN,
            count_value(*count),
            &counter.with(TOKEN_MODALITY, modality.as_str()),
        );
    }
}

/// Run a projector into a [`Projection`].
fn into_projection(
    project: impl FnOnce(&mut Vec<MetricPoint>) -> Result<Issues, MetricError>,
) -> Result<Projection, MetricError> {
    let mut points = Vec::new();
    let issues = project(&mut points)?;
    Ok(Projection {
        points,
        issues,
        ..Projection::default()
    })
}

/// Project one provider operation (`gen_ai.client.provider_attempt@0.1.0`).
pub fn project_provider_attempt(o: &ProviderAttemptObservation) -> Result<Projection, MetricError> {
    into_projection(|sink| provider_attempt(o, sink))
}

pub(crate) fn provider_attempt<'a>(
    o: &'a ProviderAttemptObservation,
    sink: &mut impl Sink<'a>,
) -> Result<Issues, MetricError> {
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

    let usage = normalize_provider_usage(&o.usage, operation == "embeddings", Some(provider))?;
    let mut issues = provider_attempt_issues(&usage);
    let base = provider_attributes(
        operation,
        provider,
        &o.request_model,
        &o.response_model,
        observation_point,
    );

    sink.histogram(
        duration_metric(operation),
        "s",
        duration,
        &base.with_some("error.type", error_type),
    );
    token_points(
        sink,
        &INPUT,
        usage.input_total,
        &usage.input_by_modality,
        base,
        tags,
        input_estimated,
    );
    token_points(
        sink,
        &OUTPUT,
        usage.output_total,
        &usage.output_by_modality,
        base,
        tags,
        output_estimated,
    );
    if let Some(seconds) = first_chunk {
        sink.histogram(
            "gen_ai.client.inference.time_to_first_chunk",
            "s",
            seconds,
            &base,
        );
    }
    if let Some(requests) = usage.web_search_requests {
        sink.counter(
            "trajectory.gen_ai.client.inference.usage.server_tool.requests",
            "{request}",
            count_value(requests),
            &base.with("trajectory.server_tool.kind", "web_search"),
        );
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
                let sourced = base
                    .with("trajectory.cost.source", cost.source)
                    .with_some(
                        "trajectory.canonical.provider",
                        o.canonical_provider.as_deref(),
                    )
                    .with_some("trajectory.canonical.model", o.canonical_model.as_deref())
                    .with_some("trajectory.canonical.family", o.model_family.as_deref());
                let counter = sourced
                    .with_some("trajectory.cost.rate_card", rate_card)
                    .with_some(
                        "trajectory.cost.priced_as",
                        o.cost_priced_as_model.as_deref(),
                    )
                    .with_some("trajectory.cost.bound", o.cost_bound.as_deref());
                sink.counter(
                    "trajectory.gen_ai.client.inference.usage.cost",
                    "{nanoUSD}",
                    count_value(nano_usd),
                    &with_price_tags(counter, tags),
                );
                sink.histogram(
                    "trajectory.gen_ai.client.operation.cost",
                    "{USD}",
                    usd,
                    &sourced,
                );
            }
        }
    }
    Ok(issues)
}

/// Project the chunk intervals of one streamed operation
/// (`gen_ai.client.provider_streaming@0.1.0`).
pub fn project_provider_streaming(
    o: &ProviderStreamingObservation,
) -> Result<Projection, MetricError> {
    into_projection(|sink| provider_streaming(o, sink))
}

pub(crate) fn provider_streaming<'a>(
    o: &'a ProviderStreamingObservation,
    sink: &mut impl Sink<'a>,
) -> Result<Issues, MetricError> {
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
    for interval in intervals {
        sink.histogram(
            "gen_ai.client.inference.time_per_output_chunk",
            "s",
            *interval,
            &base,
        );
    }
    Ok(Issues::default())
}

/// Project the cache and reasoning parts of one provider operation
/// (`trajectory.gen_ai.client.token_breakdown@0.1.0`).
pub fn project_token_breakdown(o: &TokenBreakdownObservation) -> Result<Projection, MetricError> {
    into_projection(|sink| token_breakdown(o, sink))
}

pub(crate) fn token_breakdown<'a>(
    o: &'a TokenBreakdownObservation,
    sink: &mut impl Sink<'a>,
) -> Result<Issues, MetricError> {
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

    let usage = normalize_provider_usage(&o.usage, embeddings, Some(provider))?;
    let base = provider_attributes(
        operation,
        provider,
        &o.request_model,
        &o.response_model,
        observation_point,
    );
    let base = with_price_tags(base.with(TOKEN_MODALITY, Modality::Unknown.as_str()), tags);
    let input_side = with_token_source(base, input_estimated);
    let mut counter = |name: &'static str, count: u64, attributes: &Attrs<'a>| {
        sink.counter(name, TOKEN, count_value(count), attributes);
    };
    if let Some(read) = usage.cache_read {
        counter(
            "gen_ai.client.inference.usage.cache_read.input_tokens",
            read,
            &input_side,
        );
    }
    for (lifetime, count) in [
        ("5m", usage.cache_write_5m),
        ("1h", usage.cache_write_1h),
        ("unspecified", usage.cache_write_unspecified),
    ] {
        if let Some(count) = count {
            counter(
                "gen_ai.client.inference.usage.cache_write.input_tokens",
                count,
                &input_side.with(CACHE_LIFETIME, lifetime),
            );
        }
    }
    if let Some(uncached) = usage.uncached_input {
        counter(
            "trajectory.gen_ai.client.inference.usage.uncached.input_tokens",
            uncached,
            &input_side,
        );
    }
    if let Some(reasoning) = usage.reasoning {
        counter(
            "gen_ai.client.inference.usage.reasoning.output_tokens",
            reasoning,
            &with_token_source(base, output_estimated),
        );
    }
    Ok(token_breakdown_issues(&usage))
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
    into_projection(|sink| gateway_request(o, sink))
}

pub(crate) fn gateway_request<'a>(
    o: &'a GatewayRequestObservation,
    sink: &mut impl Sink<'a>,
) -> Result<Issues, MetricError> {
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
    if let (Some(Some(nano)), Some(Some(usd))) = (estimated_from_nano, estimated_from_usd)
        && nano != usd
    {
        return reject(
            ErrorCode::FieldsContradict,
            "estimated_cost_usd and estimated_cost_nano_usd disagree",
        );
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
    let base = Attrs::new()
        .with("gen_ai.operation.name", operation)
        .with(OBSERVATION_POINT, "gateway")
        .with_some("gen_ai.request.model", o.request_model.as_deref());
    sink.histogram(
        "trajectory.gen_ai.gateway.request.duration",
        "s",
        duration,
        &base.with_some("error.type", error_type),
    );
    if let (Some(count), Some(coverage)) = (operations, coverage) {
        sink.histogram(
            "trajectory.gen_ai.gateway.request.provider_operations",
            "{operation}",
            count,
            &base.with("trajectory.provider.operation.coverage", coverage),
        );
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
            sink.histogram(name, unit, value, &base);
        }
    }
    // One Counter per outcome, in first-seen order, with the times it was seen.
    let mut seen: Vec<(&str, u64)> = Vec::new();
    for outcome in outcomes {
        match seen.iter_mut().find(|(kind, _)| *kind == outcome.as_str()) {
            Some((_, count)) => *count += 1,
            None => seen.push((outcome.as_str(), 1)),
        }
    }
    for (outcome, count) in seen {
        sink.counter(
            "trajectory.gen_ai.gateway.cache.operations",
            "{operation}",
            count_value(count),
            &base.with("trajectory.cache.outcome", outcome),
        );
    }
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
            sink.counter(
                "trajectory.gen_ai.gateway.request.estimated_cost",
                "{nanoUSD}",
                count_value(nano),
                &base,
            );
        }
    }
    Ok(issues)
}
