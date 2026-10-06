// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Bounded, borrowed input for the single-observation FFE handoff.

use crate::{FfeFlagEvaluation, FfeTelemetryContext};
use datadog_sidecar::service::blocking::{self, SidecarTransport};
pub use datadog_sidecar::service::ffe_submission::FfeSubmissionStatus;
use datadog_sidecar::service::sidecar_interface::SidecarInterfaceRequest;
use datadog_sidecar::service::{self, InstanceId, QueueId, SidecarAction};
use libdd_common_ffi::slice::{AsBytes, CharSlice, Slice};
use service::{
    ContextTruncationReason as Reason, FieldOmissions, MAX_CONTEXT_FIELDS, MAX_FIELD_LENGTH,
};

/// One borrowed scalar from the context used for evaluation. Only the first 256
/// entries are inspected, in input order, including entries subsequently omitted.
/// Only the value field selected by `kind` is read. Nothing is retained by reference.
#[repr(C)]
pub struct FfeScalarAttribute<'a> {
    pub key: CharSlice<'a>,
    /// 0 = string, 1 = boolean, 2 = signed integer, 3 = double.
    /// Other values are omitted and recorded as snapshot errors.
    pub kind: u32,
    pub string_value: CharSlice<'a>,
    pub bool_value: bool,
    pub integer_value: i64,
    pub double_value: f64,
}

/// Information lost before this native boundary. Context flags are ignored in
/// protected mode; invalid original identity is never legitimized by coercion.
#[repr(C)]
#[derive(Default)]
pub struct FfeSnapshotState {
    pub context_truncated: bool,
    pub snapshot_error: bool,
    pub targeting_key_invalid: bool,
}

/// Advisory, non-reconnecting check before preparing borrowed descriptors.
/// The caller must first honor its track kill switch and check required identity.
/// `Ready` reserves nothing: submission repeats admission. Count rejection once
/// per evaluation, not once per API call. A null transport is unavailable.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_sidecar_check_ffe_submission(
    transport: Option<&SidecarTransport>,
) -> FfeSubmissionStatus {
    transport.map_or(
        FfeSubmissionStatus::Unavailable,
        blocking::check_ffe_submission,
    )
}

/// Try to submit one evaluation; never wait for the sender, reconnect, or retain
/// rejected input. `Accepted` means local transport acceptance, not delivery.
/// The sidecar owns hashing, aggregation, final EVP encoding and HTTP delivery.
///
/// Context comes exclusively from `attributes`; `evaluation_context_json` is
/// ignored. The row must have count one and equal first/last/evaluation timestamps.
/// Protected mode does not inspect context. Invalid optional fields are omitted;
/// a snapshot failure retains the evaluation without context. Use matching headers
/// and library. No diagnostic is recursively submitted through this EVP path.
///
/// # Safety
/// All references must be valid for the call. Non-null slices that are read must
/// point to live, aligned backing storage of their stated lengths. String bytes
/// need not be UTF-8: malformed optional text is omitted. All C booleans must be
/// valid boolean values. No pointers survive this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_sidecar_try_submit_ffe_flag_evaluation(
    transport: Option<&SidecarTransport>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    context: &FfeTelemetryContext<'_>,
    event: &FfeFlagEvaluation<'_>,
    attributes: Slice<'_, FfeScalarAttribute<'_>>,
    snapshot: &FfeSnapshotState,
) -> FfeSubmissionStatus {
    let Some(transport) = transport else {
        return FfeSubmissionStatus::Unavailable;
    };
    blocking::try_submit_ffe(transport, || {
        build_request(instance_id, queue_id, context, event, attributes, snapshot)
    })
}

fn bounded_context_text<'a>(
    slice: CharSlice<'a>,
    length_reason: Reason,
) -> Result<&'a str, Reason> {
    // Four bytes per Unicode scalar; reject long input before scanning its tail.
    if slice.as_raw_parts().1 > 4 * MAX_FIELD_LENGTH {
        return Err(length_reason);
    }
    let bytes = slice.try_as_bytes().map_err(|_| Reason::SnapshotError)?;
    let text = std::str::from_utf8(bytes).map_err(|_| Reason::SnapshotError)?;
    if text.chars().take(MAX_FIELD_LENGTH + 1).count() > MAX_FIELD_LENGTH {
        return Err(length_reason);
    }
    Ok(text)
}

fn scalar_value(attribute: &FfeScalarAttribute<'_>) -> Result<serde_json::Value, Reason> {
    use serde_json::Value;
    match attribute.kind {
        0 => bounded_context_text(attribute.string_value, Reason::MaxValueLength)
            .map(|value| Value::String(value.to_owned())),
        1 => Ok(Value::Bool(attribute.bool_value)),
        2 => Ok(Value::Number(attribute.integer_value.into())),
        3 => serde_json::Number::from_f64(attribute.double_value)
            .map(Value::Number)
            .ok_or(Reason::SnapshotError),
        _ => Err(Reason::SnapshotError),
    }
}

fn capture_context(
    attributes: Slice<'_, FfeScalarAttribute<'_>>,
    snapshot: &FfeSnapshotState,
    omissions: &mut FieldOmissions,
) -> Option<String> {
    if snapshot.context_truncated || attributes.as_raw_parts().1 > MAX_CONTEXT_FIELDS {
        omissions.record_context(Reason::MaxContextFields);
    }
    if snapshot.snapshot_error {
        omissions.record_context(Reason::SnapshotError);
        return None;
    }
    let attrs = match attributes.try_as_slice() {
        Ok(attrs) => attrs,
        Err(_) => {
            omissions.record_context(Reason::SnapshotError);
            return None;
        }
    };
    let mut values = serde_json::Map::new();
    for attribute in attrs.iter().take(MAX_CONTEXT_FIELDS) {
        let entry = bounded_context_text(attribute.key, Reason::MaxKeyLength)
            .and_then(|key| scalar_value(attribute).map(|value| (key, value)));
        match entry {
            Ok((key, value)) => {
                values.insert(key.to_owned(), value);
            }
            Err(reason) => omissions.record_context(reason),
        }
    }
    if values.is_empty() {
        return None;
    }
    match serde_json::to_string(&values) {
        Ok(json) => Some(json),
        Err(_) => {
            omissions.record_context(Reason::SnapshotError);
            None
        }
    }
}

// Validate the existing packet limit before scanning or owning variable-sized
// identifiers. This is not a queue byte budget; encoded size is checked again.
fn text<'a>(slice: CharSlice<'a>) -> Result<&'a str, FfeSubmissionStatus> {
    if slice.as_raw_parts().1 > libdd_ipc::max_message_size() {
        return Err(FfeSubmissionStatus::PayloadTooLarge);
    }
    let bytes = slice
        .try_as_bytes()
        .map_err(|_| FfeSubmissionStatus::InvalidInput)?;
    std::str::from_utf8(bytes).map_err(|_| FfeSubmissionStatus::InvalidInput)
}

fn optional_text(slice: CharSlice<'_>) -> Result<Option<&str>, FfeSubmissionStatus> {
    match text(slice) {
        Ok("") | Err(FfeSubmissionStatus::InvalidInput) => Ok(None),
        Ok(text) => Ok(Some(text)),
        Err(status) => Err(status),
    }
}

fn safe_error(slice: CharSlice<'_>) -> Option<service::EvalError> {
    if slice.as_raw_parts().1 == 0 {
        return None;
    }
    // Long errors cannot be a canonical code. Do not scan or copy their body.
    if slice.as_raw_parts().1 > 32 {
        return service::EvalError::from_message("GENERAL");
    }
    let message = slice
        .try_as_bytes()
        .ok()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or("GENERAL");
    service::EvalError::from_message(message)
}

fn build_request(
    instance_id: &InstanceId,
    queue_id: &QueueId,
    context: &FfeTelemetryContext<'_>,
    source: &FfeFlagEvaluation<'_>,
    attributes: Slice<'_, FfeScalarAttribute<'_>>,
    snapshot: &FfeSnapshotState,
) -> Result<SidecarInterfaceRequest, FfeSubmissionStatus> {
    if source.evaluation_count != 1
        || source.first_evaluation_ms != source.timestamp_ms
        || source.last_evaluation_ms != source.timestamp_ms
    {
        return Err(FfeSubmissionStatus::InvalidInput);
    }
    let service = text(context.service)?;
    let env = text(context.env)?;
    let version = text(context.version)?;
    let flag = text(source.flag_key)?;
    let variant = optional_text(source.variant)?;
    let allocation = optional_text(source.allocation_key)?;
    let rule = optional_text(source.targeting_rule_key)?;
    let mut omissions = FieldOmissions::default();
    omissions.targeting_key_invalid = snapshot.targeting_key_invalid;
    let target = if snapshot.targeting_key_invalid
        || source.targeting_key.as_raw_parts() == (std::ptr::null(), 0)
    {
        None
    } else {
        match text(source.targeting_key) {
            Ok(target) => Some(target),
            Err(FfeSubmissionStatus::InvalidInput) => {
                omissions.targeting_key_invalid = true;
                None
            }
            Err(status) => return Err(status),
        }
    };
    let mut bytes = 0usize;
    for value in [
        instance_id.session_id.as_str(),
        instance_id.runtime_id.as_str(),
        service,
        service,
        env,
        version,
        flag,
        variant.unwrap_or_default(),
        allocation.unwrap_or_default(),
        rule.unwrap_or_default(),
        target.unwrap_or_default(),
    ] {
        bytes = bytes
            .checked_add(value.len())
            .ok_or(FfeSubmissionStatus::PayloadTooLarge)?;
        if bytes > libdd_ipc::max_message_size() {
            return Err(FfeSubmissionStatus::PayloadTooLarge);
        }
    }
    let evaluation = if source.observe_full_evaluation_data {
        capture_context(attributes, snapshot, &mut omissions)
    } else {
        None
    };
    let dd = (!service.is_empty()).then(|| service::ContextDD {
        service: service.to_owned(),
    });
    let event = service::FfeFlagEvaluationEvent {
        timestamp: source.timestamp_ms,
        flag: service::FlagKey {
            key: flag.to_owned(),
        },
        first_evaluation: source.timestamp_ms,
        last_evaluation: source.timestamp_ms,
        evaluation_count: 1,
        variant: variant.map(|key| service::VariantKey {
            key: key.to_owned(),
        }),
        allocation: allocation.map(|key| service::AllocationKey {
            key: key.to_owned(),
        }),
        targeting_rule: rule.map(|key| service::TargetingRuleKey {
            key: key.to_owned(),
        }),
        targeting_key: target.map(str::to_owned),
        context: (evaluation.is_some() || dd.is_some())
            .then_some(service::FlagEvalEventContext { evaluation, dd }),
        error: safe_error(source.error_message),
        runtime_default_used: source.runtime_default_used,
        observe_full_evaluation_data: source.observe_full_evaluation_data,
        is_degraded: false,
        field_omissions: omissions,
    };
    // Context and errors are already sanitized above; the sidecar independently
    // validates them again at aggregation, without another JSON pass here.
    Ok(SidecarInterfaceRequest::EnqueueActions {
        instance_id: instance_id.clone(),
        queue_id: *queue_id,
        actions: vec![SidecarAction::FfeFlagEvaluationBatch(
            service::FfeFlagEvaluationBatch {
                context: service::FfeTelemetryContext {
                    service: service.to_owned(),
                    env: env.to_owned(),
                    version: version.to_owned(),
                },
                flag_evaluations: vec![event],
            },
        )],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FfeFlagEvaluation, FfeTelemetryContext};
    use datadog_sidecar::service::{ContextTruncationReason as Reason, SidecarAction};
    use serde_json::{Value, json};

    fn metadata() -> FfeTelemetryContext<'static> {
        FfeTelemetryContext {
            service: "svc".into(),
            env: "test".into(),
            version: "1".into(),
        }
    }

    fn event(consent: bool) -> FfeFlagEvaluation<'static> {
        FfeFlagEvaluation {
            timestamp_ms: 100,
            first_evaluation_ms: 100,
            last_evaluation_ms: 100,
            evaluation_count: 1,
            flag_key: "flag".into(),
            variant: "blue".into(),
            allocation_key: "allocation".into(),
            targeting_rule_key: "".into(),
            targeting_key: "jane@example.com".into(),
            evaluation_context_json: "private-json-canary".into(),
            error_message: "".into(),
            runtime_default_used: false,
            observe_full_evaluation_data: consent,
        }
    }

    fn attr<'a>(key: &'a str, value: &'a str) -> FfeScalarAttribute<'a> {
        FfeScalarAttribute {
            key: key.into(),
            kind: 0,
            string_value: value.into(),
            bool_value: false,
            integer_value: 0,
            double_value: 0.0,
        }
    }

    fn convert(
        event: &FfeFlagEvaluation<'_>,
        attrs: &[FfeScalarAttribute<'_>],
        state: FfeSnapshotState,
    ) -> service::FfeFlagEvaluationEvent {
        let request = build_request(
            &InstanceId::new("session", "runtime"),
            &QueueId::from(1),
            &metadata(),
            event,
            attrs.into(),
            &state,
        )
        .unwrap();
        let SidecarInterfaceRequest::EnqueueActions { mut actions, .. } = request else {
            panic!("wrong request")
        };
        let SidecarAction::FfeFlagEvaluationBatch(mut batch) = actions.remove(0) else {
            panic!("wrong action")
        };
        assert_eq!(batch.flag_evaluations.len(), 1);
        batch.flag_evaluations.remove(0)
    }

    fn context(event: &service::FfeFlagEvaluationEvent) -> Value {
        serde_json::from_str(event.context.as_ref().unwrap().evaluation.as_ref().unwrap()).unwrap()
    }

    #[test]
    fn scalar_types_and_verbatim_values_are_captured_without_legacy_json() {
        let mut attrs = vec![
            attr("text", " original "),
            attr("boolean", ""),
            attr("integer", ""),
            attr("double", ""),
        ];
        attrs[1].kind = 1;
        attrs[1].bool_value = true;
        attrs[2].kind = 2;
        attrs[2].integer_value = -42;
        attrs[3].kind = 3;
        attrs[3].double_value = 1.25;
        let row = convert(&event(true), &attrs, FfeSnapshotState::default());
        assert_eq!(
            context(&row),
            json!({"text":" original ", "boolean":true,"integer":-42,"double":1.25})
        );
        assert_eq!(row.targeting_key.as_deref(), Some("jane@example.com"));
        assert!(row.observe_full_evaluation_data);
    }

    #[test]
    fn protected_capture_ignores_context_and_its_error_state() {
        let mut attribute = attr("invalid", "private-context-canary");
        attribute.kind = u32::MAX;
        let row = convert(
            &event(false),
            &[attribute],
            FfeSnapshotState {
                context_truncated: true,
                snapshot_error: true,
                targeting_key_invalid: false,
            },
        );
        assert!(row.context.unwrap().evaluation.is_none());
        assert!(!row.field_omissions.contains_context(Reason::SnapshotError));
        assert!(
            !row.field_omissions
                .contains_context(Reason::MaxContextFields)
        );
    }

    #[test]
    fn malformed_slice_is_ignored_when_protected_and_recovers_when_consented() {
        // Emulate a malformed C descriptor. try_as_slice must reject its null
        // pointer before forming a Rust slice; protected mode never inspects it.
        for consent in [false, true] {
            let attributes = unsafe { Slice::from_raw_parts(std::ptr::null(), 1) };
            let request = build_request(
                &InstanceId::new("session", "runtime"),
                &QueueId::from(1),
                &metadata(),
                &event(consent),
                attributes,
                &FfeSnapshotState::default(),
            )
            .unwrap();
            let SidecarInterfaceRequest::EnqueueActions { actions, .. } = request else {
                panic!("wrong request")
            };
            let SidecarAction::FfeFlagEvaluationBatch(batch) = &actions[0] else {
                panic!("wrong action")
            };
            let row = &batch.flag_evaluations[0];
            assert!(row.context.as_ref().unwrap().evaluation.is_none());
            assert_eq!(
                row.field_omissions.contains_context(Reason::SnapshotError),
                consent
            );
            assert_eq!(row.evaluation_count, 1);
        }
    }

    #[test]
    fn only_first_256_entries_are_inspected_even_when_one_is_rejected() {
        let names: Vec<_> = (0..257).map(|i| format!("field{i}")).collect();
        let mut attrs: Vec<_> = names.iter().map(|name| attr(name, "value")).collect();
        attrs[0].kind = u32::MAX;
        let row = convert(&event(true), &attrs, FfeSnapshotState::default());
        let value = context(&row);
        assert_eq!(value.as_object().unwrap().len(), 255);
        assert!(value.get("field0").is_none());
        assert!(value.get("field256").is_none());
        assert_eq!(value["field255"], "value");
        assert!(
            row.field_omissions
                .contains_context(Reason::MaxContextFields)
        );
    }

    #[test]
    fn scalar_and_json_entry_points_agree_on_top_level_truncation() {
        let overlength = "x".repeat(257);
        for count in [256, 257] {
            for reject_first in [false, true] {
                let names: Vec<_> = (0..count).map(|i| format!("field{i:03}")).collect();
                let values: Vec<_> = (0..count)
                    .map(|i| {
                        if reject_first && i == 0 {
                            overlength.as_str()
                        } else {
                            "kept"
                        }
                    })
                    .collect();
                let attrs: Vec<_> = names
                    .iter()
                    .zip(&values)
                    .map(|(key, value)| attr(key, value))
                    .collect();
                let json = serde_json::to_string(
                    &names
                        .iter()
                        .zip(&values)
                        .collect::<std::collections::BTreeMap<_, _>>(),
                )
                .unwrap();
                let mut source = event(true);
                source.evaluation_context_json = json.as_str().into();
                let scalar = convert(&source, &attrs, FfeSnapshotState::default());
                let legacy = crate::ffe_flag_evaluation_from_ffi(&source, "svc").unwrap();
                let mut expected = FieldOmissions::default();
                if count > 256 {
                    expected.record_context(Reason::MaxContextFields);
                }
                if reject_first {
                    expected.record_context(Reason::MaxValueLength);
                }
                assert_eq!(scalar.field_omissions, expected);
                assert_eq!(legacy.field_omissions, expected);
                assert_eq!(context(&scalar), context(&legacy));
                assert_eq!(
                    context(&scalar).as_object().unwrap().len(),
                    256 - usize::from(reject_first)
                );
            }
        }
    }

    #[test]
    fn unicode_limits_skip_instead_of_truncating() {
        let exact = "é".repeat(256);
        let over = "é".repeat(257);
        let row = convert(
            &event(true),
            &[
                attr(&exact, &exact),
                attr(&over, "value"),
                attr("over", &over),
                attr("", ""),
            ],
            FfeSnapshotState::default(),
        );
        let value = context(&row);
        assert_eq!(value[&exact], exact);
        assert!(value.get(&over).is_none());
        assert!(value.get("over").is_none());
        assert!(row.field_omissions.contains_context(Reason::MaxKeyLength));
        assert!(row.field_omissions.contains_context(Reason::MaxValueLength));
    }

    #[test]
    fn bad_scalars_are_omitted_without_losing_healthy_fields() {
        let invalid = [0xffu8];
        let mut bad = attr("bad", "");
        bad.string_value = CharSlice::from_bytes(&invalid);
        let mut number = attr("number", "");
        number.kind = 3;
        number.double_value = f64::INFINITY;
        let row = convert(
            &event(true),
            &[attr("good", "kept"), bad, number],
            FfeSnapshotState::default(),
        );
        assert_eq!(context(&row), json!({"good":"kept"}));
        assert!(row.field_omissions.contains_context(Reason::SnapshotError));
    }

    #[test]
    fn caller_snapshot_failure_keeps_evaluation_without_context() {
        let row = convert(
            &event(true),
            &[attr("good", "value")],
            FfeSnapshotState {
                snapshot_error: true,
                ..Default::default()
            },
        );
        assert!(row.context.unwrap().evaluation.is_none());
        assert_eq!(row.evaluation_count, 1);
        assert!(row.observe_full_evaluation_data);
        assert!(row.field_omissions.contains_context(Reason::SnapshotError));
        let row = convert(
            &event(true),
            &[attr("good", "value")],
            FfeSnapshotState {
                context_truncated: true,
                ..Default::default()
            },
        );
        assert_eq!(context(&row)["good"], "value");
        assert!(
            row.field_omissions
                .contains_context(Reason::MaxContextFields)
        );
    }

    #[test]
    fn invalid_original_identity_is_not_legitimized_by_coercion() {
        let row = convert(
            &event(false),
            &[],
            FfeSnapshotState {
                targeting_key_invalid: true,
                ..Default::default()
            },
        );
        assert!(row.targeting_key.is_none());
        assert!(row.field_omissions.targeting_key_invalid);
        let mut source = event(false);
        source.targeting_key = "".into();
        assert_eq!(
            convert(&source, &[], FfeSnapshotState::default())
                .targeting_key
                .as_deref(),
            Some("")
        );
        source.targeting_key = unsafe { CharSlice::from_raw_parts(std::ptr::null(), 0) };
        let row = convert(&source, &[], FfeSnapshotState::default());
        assert!(row.targeting_key.is_none());
        assert!(!row.field_omissions.targeting_key_invalid);
    }

    #[test]
    fn invalid_optional_text_and_private_errors_do_not_drop_the_event() {
        let invalid = [0xffu8];
        let mut source = event(true);
        source.variant = CharSlice::from_bytes(&invalid);
        source.allocation_key = source.variant;
        source.targeting_rule_key = source.variant;
        source.targeting_key = source.variant;
        for (input, expected) in [
            ("FLAG_NOT_FOUND", Some("FLAG_NOT_FOUND")),
            ("private-error-canary", Some("GENERAL")),
            ("", None),
        ] {
            source.error_message = input.into();
            let row = convert(&source, &[], FfeSnapshotState::default());
            assert!(
                row.variant.is_none() && row.allocation.is_none() && row.targeting_rule.is_none()
            );
            assert!(row.targeting_key.is_none());
            assert!(row.field_omissions.targeting_key_invalid);
            assert_eq!(row.error.as_ref().map(|e| e.message.as_str()), expected);
        }
    }

    #[test]
    fn error_capture_preserves_canonical_codes_and_sanitizes_malformed_input() {
        let mut source = event(false);
        for (input, expected) in [
            ("PROVIDER_NOT_READY", Some("PROVIDER_NOT_READY")),
            ("PROVIDER_FATAL", Some("PROVIDER_FATAL")),
            ("FLAG_NOT_FOUND", Some("FLAG_NOT_FOUND")),
            ("PARSE_ERROR", Some("PARSE_ERROR")),
            ("TYPE_MISMATCH", Some("TYPE_MISMATCH")),
            ("TARGETING_KEY_MISSING", Some("TARGETING_KEY_MISSING")),
            ("INVALID_CONTEXT", Some("INVALID_CONTEXT")),
            ("GENERAL", Some("GENERAL")),
            ("flag_not_found", Some("GENERAL")),
            (" FLAG_NOT_FOUND ", Some("GENERAL")),
            ("", None),
        ] {
            source.error_message = input.into();
            let row = convert(&source, &[], FfeSnapshotState::default());
            assert_eq!(row.error.as_ref().map(|e| e.message.as_str()), expected);
        }
        let invalid = [0xffu8];
        for input in [
            CharSlice::from_bytes(&invalid),
            // Malformed descriptors must not be mistaken for an absent error.
            unsafe { CharSlice::from_raw_parts(std::ptr::null(), 1) },
            unsafe { CharSlice::from_raw_parts(std::ptr::null(), 33) },
        ] {
            source.error_message = input;
            let row = convert(&source, &[], FfeSnapshotState::default());
            assert_eq!(row.error.unwrap().message, "GENERAL");
        }
    }

    #[test]
    fn captured_context_needs_no_further_pruning() {
        let over = "é".repeat(257);
        let mut attrs = vec![attr("kept", "é"), attr("omitted", &over), attr("min", "")];
        attrs[2].kind = 2;
        attrs[2].integer_value = i64::MIN;
        for consent in [false, true] {
            for snapshot_error in [false, true] {
                let mut row = convert(
                    &event(consent),
                    &attrs,
                    FfeSnapshotState {
                        context_truncated: true,
                        snapshot_error,
                        targeting_key_invalid: false,
                    },
                );
                if consent && !snapshot_error {
                    assert_eq!(context(&row), json!({"kept":"é", "min":i64::MIN}));
                } else {
                    assert!(row.context.as_ref().unwrap().evaluation.is_none());
                }
                let before = serde_json::to_value(&row).unwrap();
                let omissions = row.field_omissions;
                row.normalize();
                assert_eq!(serde_json::to_value(&row).unwrap(), before);
                assert_eq!(row.field_omissions, omissions);
            }
        }
    }

    #[test]
    fn aggregated_input_is_rejected() {
        let mut source = event(false);
        source.evaluation_count = 2;
        let result = build_request(
            &InstanceId::new("s", "r"),
            &QueueId::from(1),
            &metadata(),
            &source,
            Slice::empty(),
            &FfeSnapshotState::default(),
        );
        assert_eq!(result.unwrap_err(), FfeSubmissionStatus::InvalidInput);
    }

    #[test]
    fn oversized_identifiers_reject_but_ignored_error_and_context_do_not() {
        let huge = "x".repeat(libdd_ipc::max_message_size() + 1);
        let mut source = event(false);
        source.flag_key = huge.as_str().into();
        let result = build_request(
            &InstanceId::new("s", "r"),
            &QueueId::from(1),
            &metadata(),
            &source,
            Slice::empty(),
            &FfeSnapshotState::default(),
        );
        assert_eq!(result.unwrap_err(), FfeSubmissionStatus::PayloadTooLarge);
        source.flag_key = "flag".into();
        source.error_message = huge.as_str().into();
        source.evaluation_context_json = huge.as_str().into();
        let row = convert(&source, &[], FfeSnapshotState::default());
        assert_eq!(row.error.unwrap().message, "GENERAL");
        assert!(row.context.unwrap().evaluation.is_none());
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn ffi_rechecks_admission_and_owns_the_accepted_snapshot() {
        let (conn, peer) = libdd_ipc::SeqpacketConn::socketpair().unwrap();
        let transport = SidecarTransport::from(conn);
        let instance = InstanceId::new("session", "runtime");
        let queue = QueueId::from(1);
        assert_eq!(
            ddog_sidecar_check_ffe_submission(Some(&transport)),
            FfeSubmissionStatus::Ready
        );
        let mut source = event(true);
        let mut value = String::from("captured");
        let guard = transport.inner.lock().unwrap();
        let send = |attrs: &[FfeScalarAttribute<'_>]| unsafe {
            ddog_sidecar_try_submit_ffe_flag_evaluation(
                Some(&transport),
                &instance,
                &queue,
                &metadata(),
                &source,
                attrs.into(),
                &FfeSnapshotState::default(),
            )
        };
        assert_eq!(send(&[attr("key", &value)]), FfeSubmissionStatus::Busy);
        drop(guard);
        assert_eq!(send(&[attr("key", &value)]), FfeSubmissionStatus::Accepted);
        value.clear();
        source.observe_full_evaluation_data = false;
        assert!(value.is_empty() && !source.observe_full_evaluation_data);
        let mut buf = vec![0; libdd_ipc::max_message_size()];
        let (len, _) = peer.try_recv_raw(&mut buf).unwrap();
        let decoded: SidecarInterfaceRequest = libdd_ipc::codec::decode(&buf[..len]).unwrap();
        let SidecarInterfaceRequest::EnqueueActions { actions, .. } = decoded else {
            panic!("wrong request")
        };
        let SidecarAction::FfeFlagEvaluationBatch(batch) = &actions[0] else {
            panic!("wrong action")
        };
        assert_eq!(
            context(&batch.flag_evaluations[0]),
            json!({"key":"captured"})
        );
        assert!(batch.flag_evaluations[0].observe_full_evaluation_data);
        assert_eq!(
            ddog_sidecar_check_ffe_submission(None),
            FfeSubmissionStatus::Unavailable
        );
    }
}
