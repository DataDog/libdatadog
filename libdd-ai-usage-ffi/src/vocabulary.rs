// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The vocabularies every integration shares: provider names, operation
//! names, `error.type`, metric names, tag values, provider ids and the
//! grammars of attribute values.

use libdd_ai_usage::{
    Json, classify_error, classify_known_error, datadog_tag_value, gen_ai_provider_name,
    is_deployment_value, is_person_identifier, is_service_tier, known_metrics, operation_name_for,
    provider_id_headers, provider_ids_from_json,
};
use libdd_common_ffi::{CharSlice, StringWrapper};
use serde_json::json;

use crate::{
    AiUsageError, Failure, catch_panic, invalid, panicked_output, parse_document, text,
    write_output,
};

/// The UTF-8 text of an argument, or an internal failure naming it.
fn text_arg<'a>(slice: CharSlice<'a>, what: &'static str) -> Result<&'a str, Failure> {
    text(slice).ok_or(Failure::Internal(what))
}

/// Write the `gen_ai.provider.name` of the provider `name` (a gateway's or
/// SDK's name for it, such as `bedrock` or `vertex_ai`) to `out`. An error
/// with code `NO_MATCH` when no identifier can be formed.
///
/// # Safety
///
/// `name` must be a valid slice, and `out` null or point to a
/// `ddog_StringWrapper` the caller releases with `ddog_StringWrapper_drop`.
/// `out` is written whether the call fails or not (empty when it fails).
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_provider_name(
    name: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                gen_ai_provider_name(text_arg(name, "name is not UTF-8")?)
                    .ok_or(Failure::NoMatch("no provider identifier can be formed"))
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Write the `gen_ai.operation.name` of a gateway route or request path
/// `route` to `out`: an upstream path (`/v1/chat/completions`,
/// `/v1/messages`, `/v1/responses`, `/v1/embeddings`, `/v1/completions`,
/// Gemini's `:generateContent`, Bedrock's `/converse`; a query string is
/// ignored), or an operation name. An error with code `NO_MATCH` for
/// anything else.
///
/// # Safety
///
/// As [`ddog_ai_usage_provider_name`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_operation_name(
    route: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                operation_name_for(text_arg(route, "route is not UTF-8")?)
                    .map(str::to_string)
                    .ok_or(Failure::NoMatch("the route names no operation"))
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Write the `error.type` of a failure to `out`: from its error name `name`
/// (an SDK exception class, or the error type or code a provider reported;
/// may be empty) and its HTTP `status` (0 when there is none). An error with
/// code `NO_MATCH` when neither says the operation failed.
///
/// # Safety
///
/// As [`ddog_ai_usage_provider_name`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_error_type(
    name: CharSlice,
    status: u16,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let name = text_arg(name, "name is not UTF-8")?;
                classify_error(Some(name), (status != 0).then_some(status))
                    .map(str::to_string)
                    .ok_or(Failure::NoMatch(
                        "neither the name nor the status is a failure",
                    ))
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// [`ddog_ai_usage_error_type`] without its fallback: `NO_MATCH` also for a
/// name the vocabulary does not know and no failed `status`, which
/// [`ddog_ai_usage_error_type`] writes as `api_error`. So a caller can tell
/// an `api_error` the provider reported from one that only means "failed,
/// cause unknown", and keep its own label for the unknown name.
///
/// # Safety
///
/// As [`ddog_ai_usage_provider_name`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_error_type_known(
    name: CharSlice,
    status: u16,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let name = text_arg(name, "name is not UTF-8")?;
                classify_known_error(Some(name), (status != 0).then_some(status))
                    .map(str::to_string)
                    .ok_or(Failure::NoMatch(
                        "the name is not known and the status is not a failure",
                    ))
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// The metrics of the profile `profile` (`{id}@{version}`), or of every
/// profile this library implements when it is empty, as the registry names
/// them: writes `["gen_ai.client.inference.duration", ...]`, the names a
/// request document's `metrics` list can keep. An unknown profile is
/// rejected with `profile_invalid`.
///
/// # Safety
///
/// As [`ddog_ai_usage_provider_name`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_known_metrics(
    profile: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let id = text_arg(profile, "profile is not UTF-8")?;
                Ok(json!(known_metrics(id)?).to_string())
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Write `value` normalized as Datadog stores a tag value to `out`.
///
/// # Safety
///
/// As [`ddog_ai_usage_provider_name`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_datadog_tag_value(
    value: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                Ok(datadog_tag_value(text_arg(value, "value is not UTF-8")?))
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// The lower-case names of the response headers
/// [`ddog_ai_usage_provider_ids`] reads for the provider `provider_name` (a
/// `gen_ai.provider.name`, such as `openai`, as text or as a JSON string), so
/// a gateway fetches exactly those: writes `["openai-organization",
/// "openai-project"]`. An empty name lists the headers of every provider; a
/// provider whose ids come from no header has none. A JSON object or array
/// is rejected.
///
/// # Safety
///
/// As [`ddog_ai_usage_provider_name`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_provider_id_headers(
    provider_name: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let raw = text_arg(provider_name, "provider_name is not UTF-8")?;
                // The name as text, or as a JSON string like the other calls
                // take; a JSON object or array is a mistake, not a provider.
                let decoded;
                let provider = match raw.trim_start().as_bytes().first() {
                    Some(b'"') => {
                        decoded = Json::parse(raw)
                            .ok()
                            .and_then(|name| name.as_str().map(str::to_string))
                            .ok_or_else(|| invalid("provider_name is not a JSON string"))?;
                        decoded.as_str()
                    }
                    Some(b'{' | b'[') => {
                        return Err(invalid(
                            "provider_name must be a provider name, not a JSON object or array",
                        ));
                    }
                    _ => raw,
                };
                Ok(json!(provider_id_headers(provider)).to_string())
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// The deployment attributes a provider's response names, from the request
/// document `input`: `{"provider_name": "anthropic", "headers":
/// {"anthropic-organization-id": "..."}, "models": ["arn:..."],
/// "upstream_url": "https://...aiplatform.googleapis.com/v1/projects/{p}/..."}`.
/// Header names are matched in lower case. Writes `{attribute: value}`:
/// `trajectory.request.provider_account_id` and
/// `trajectory.request.project_id` from the provider's headers, or for
/// `aws.bedrock` the account a model ARN names, and the Vertex AI project
/// an `upstream_url` path names as `trajectory.request.project_id` when no
/// header named one. Without a `provider_name` (absent or empty), every
/// known header and the model ARNs are tried, as
/// [`ddog_ai_usage_provider_id_headers`] lists them for an empty name.
///
/// # Safety
///
/// As [`ddog_ai_usage_provider_name`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_provider_ids(
    input: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let document = parse_document(input)?;
                Ok(json!(provider_ids_from_json(&document)).to_string())
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Whether `value` is a service tier an observation can carry; false for
/// text that is not UTF-8.
///
/// # Safety
///
/// `value` must be a valid slice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_is_service_tier(value: CharSlice) -> bool {
    catch_panic!(text(value).is_some_and(is_service_tier), false)
}

/// Whether `value` is kept as a deployment attribute value; false for text
/// that is not UTF-8.
///
/// # Safety
///
/// `value` must be a valid slice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_is_deployment_value(value: CharSlice) -> bool {
    catch_panic!(text(value).is_some_and(is_deployment_value), false)
}

/// Whether `value` names a person (an email address), the only value
/// `user.id` may carry; false for text that is not UTF-8.
///
/// # Safety
///
/// `value` must be a valid slice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_is_person_identifier(value: CharSlice) -> bool {
    catch_panic!(text(value).is_some_and(is_person_identifier), false)
}
