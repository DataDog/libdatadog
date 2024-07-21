// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::CharSlice;
use std::borrow::Cow;
use std::collections::hash_map;
use std::mem::transmute;
// Alias to prevent cbindgen panic
use crate::data::Probe;
use datadog_live_debugger::debugger_defs::{Capture as DebuggerCaptureAlias, Capture, Captures, DebuggerData, DebuggerPayload, Diagnostics, DiagnosticsError, Entry, Fields, ProbeStatus, Snapshot, SnapshotEvaluationError, Value as DebuggerValueAlias};
use datadog_live_debugger::sender::generate_new_id;
use datadog_live_debugger::{
    add_redacted_name, add_redacted_type, is_redacted_name, is_redacted_type,
};
use ddcommon_ffi::slice::AsBytes;

#[repr(C)]
pub enum FieldType {
    STATIC,
    ARG,
    LOCAL,
}

#[repr(C)]
pub struct CaptureValue<'a> {
    pub r#type: CharSlice<'a>,
    pub value: CharSlice<'a>,
    pub fields: Option<Box<Fields<'a>>>,
    pub elements: Vec<DebuggerValue<'a>>,
    pub entries: Vec<Entry<'a>>,
    pub is_null: bool,
    pub truncated: bool,
    pub not_captured_reason: CharSlice<'a>,
    pub size: CharSlice<'a>,
}

impl<'a> From<CaptureValue<'a>> for DebuggerValueAlias<'a> {
    fn from(val: CaptureValue<'a>) -> Self {
        DebuggerValueAlias {
            r#type: val.r#type.to_utf8_lossy(),
            value: if val.value.len() == 0 {
                None
            } else {
                Some(val.value.to_utf8_lossy())
            },
            fields: if let Some(boxed) = val.fields {
                *boxed
            } else {
                Fields::default()
            },
            elements: unsafe { transmute(val.elements) }, // SAFETY: is transparent
            entries: val.entries,
            is_null: val.is_null,
            truncated: val.truncated,
            not_captured_reason: if val.not_captured_reason.len() == 0 {
                None
            } else {
                Some(val.not_captured_reason.to_utf8_lossy())
            },
            size: if val.size.len() == 0 {
                None
            } else {
                Some(val.size.to_utf8_lossy())
            },
        }
    }
}

/// cbindgen:no-export
#[repr(transparent)]
pub struct DebuggerValue<'a>(DebuggerValueAlias<'a>);
/// cbindgen:no-export
#[repr(transparent)]
pub struct DebuggerCapture<'a>(DebuggerCaptureAlias<'a>);

#[repr(C)]
pub struct ExceptionSnapshot<'a> {
    pub data: *mut DebuggerPayload<'a>,
    pub capture: *mut DebuggerCapture<'a>,
}

#[no_mangle]
pub extern "C" fn ddog_create_exception_snapshot<'a>(
    buffer: &mut Vec<DebuggerPayload<'a>>,
    service: CharSlice<'a>,
    language: CharSlice<'a>,
    id: CharSlice<'a>,
    exception_id: CharSlice<'a>,
    timestamp: u64,
) -> *mut DebuggerCapture<'a> {
    let snapshot = DebuggerPayload {
        service: service.to_utf8_lossy(),
        source: "dd_debugger",
        timestamp,
        message: None,
        debugger: DebuggerData::Snapshot(Snapshot {
            captures: Some(Captures {
                r#return: Some(Capture::default()),
                ..Default::default()
            }),
            language: language.to_utf8_lossy(),
            id: id.to_utf8_lossy(),
            exception_id: Some(exception_id.to_utf8_lossy()),
            timestamp,
            ..Default::default()
        }),
    };
    buffer.push(snapshot);
    let DebuggerData::Snapshot(ref mut snapshot) = buffer.last_mut().unwrap().debugger else { unreachable!(); };
    unsafe { transmute(snapshot.captures.as_mut().unwrap().r#return.as_mut().unwrap()) }
}

#[no_mangle]
pub extern "C" fn ddog_create_log_probe_snapshot<'a>(
    probe: &'a Probe,
    message: Option<&CharSlice<'a>>,
    service: CharSlice<'a>,
    language: CharSlice<'a>,
    timestamp: u64,
) -> Box<DebuggerPayload<'a>> {
    Box::new(DebuggerPayload {
        service: service.to_utf8_lossy(),
        source: "dd_debugger",
        timestamp,
        message: message.map(|m| m.to_utf8_lossy()),
        debugger: DebuggerData::Snapshot(Snapshot {
            captures: Some(Captures {
                ..Default::default()
            }),
            language: language.to_utf8_lossy(),
            id: Cow::Owned(generate_new_id().as_hyphenated().to_string()),
            probe: Some(probe.into()),
            timestamp,
            ..Default::default()
        }),
    })
}

#[no_mangle]
pub extern "C" fn ddog_update_payload_message<'a>(
    payload: &mut DebuggerPayload<'a>,
    message: CharSlice<'a>,
) {
    payload.message = Some(message.to_utf8_lossy());
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_snapshot_entry<'a>(
    payload: &mut DebuggerPayload<'a>,
) -> *mut DebuggerCapture<'a> {
    let DebuggerData::Snapshot(ref mut snapshot) = payload.debugger else { unreachable!(); };
    transmute(snapshot.captures.as_mut().unwrap().entry.insert(Capture::default()))
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_snapshot_lines<'a>(
    payload: &mut DebuggerPayload<'a>,
    line: u32,
) -> *mut DebuggerCapture<'a> {
    let DebuggerData::Snapshot(ref mut snapshot) = payload.debugger else { unreachable!(); };
    transmute(match snapshot.captures.as_mut().unwrap().lines.entry(line) {
        hash_map::Entry::Occupied(e) => e.into_mut(),
        hash_map::Entry::Vacant(e) => e.insert(Capture::default()),
    })
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_snapshot_exit<'a>(
    payload: &mut DebuggerPayload<'a>,
) -> *mut DebuggerCapture<'a> {
    let DebuggerData::Snapshot(ref mut snapshot) = payload.debugger else { unreachable!(); };
    transmute(snapshot.captures.as_mut().unwrap().r#return.insert(Capture::default()))
}

#[no_mangle]
pub extern "C" fn ddog_snapshot_redacted_name(name: CharSlice) -> bool {
    is_redacted_name(name.as_bytes())
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_snapshot_add_redacted_name(name: CharSlice) {
    add_redacted_name(name.as_bytes())
}

#[no_mangle]
pub extern "C" fn ddog_snapshot_redacted_type(name: CharSlice) -> bool {
    is_redacted_type(name.as_bytes())
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_snapshot_add_redacted_type(name: CharSlice) {
    add_redacted_type(name.as_bytes())
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_snapshot_add_field<'a, 'b: 'a, 'c: 'a>(
    capture: &mut DebuggerCapture<'a>,
    r#type: FieldType,
    name: CharSlice<'b>,
    value: CaptureValue<'c>,
) {
    let fields = match r#type {
        FieldType::STATIC => &mut capture.0.static_fields,
        FieldType::ARG => &mut capture.0.arguments,
        FieldType::LOCAL => &mut capture.0.locals,
    };
    fields.insert(name.to_utf8_lossy(), value.into());
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_capture_value_add_element<'a, 'b: 'a>(
    value: &mut CaptureValue<'a>,
    element: CaptureValue<'b>,
) {
    value.elements.push(DebuggerValue(element.into()));
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_capture_value_add_entry<'a, 'b: 'a, 'c: 'a>(
    value: &mut CaptureValue<'a>,
    key: CaptureValue<'b>,
    element: CaptureValue<'c>,
) {
    value.entries.push(Entry(key.into(), element.into()));
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_capture_value_add_field<'a, 'b: 'a, 'c: 'a>(
    value: &mut CaptureValue<'a>,
    key: CharSlice<'b>,
    element: CaptureValue<'c>,
) {
    let fields = match value.fields {
        None => {
            value.fields = Some(Box::default());
            value.fields.as_mut().unwrap()
        }
        Some(ref mut f) => f,
    };
    fields.insert(key.to_utf8_lossy(), element.into());
}

#[no_mangle]
pub extern "C" fn ddog_snapshot_format_new_uuid(buf: &mut [u8; 36]) {
    generate_new_id().as_hyphenated().encode_lower(buf);
}

#[no_mangle]
pub extern "C" fn ddog_evaluation_error_first_msg(vec: &Vec<SnapshotEvaluationError>) -> CharSlice {
    CharSlice::from(vec[0].message.as_str())
}

#[no_mangle]
pub extern "C" fn ddog_evaluation_error_drop(_: Box<Vec<SnapshotEvaluationError>>) {}

#[no_mangle]
pub extern "C" fn ddog_evaluation_error_snapshot<'a>(
    probe: &'a Probe,
    service: CharSlice<'a>,
    language: CharSlice<'a>,
    errors: Box<Vec<SnapshotEvaluationError>>,
    timestamp: u64,
) -> Box<DebuggerPayload<'a>> {
    Box::new(DebuggerPayload {
        service: service.to_utf8_lossy(),
        source: "dd_debugger",
        timestamp,
        message: Some(Cow::Owned(format!(
            "Evaluation errors for probe id {}",
            probe.id
        ))),
        debugger: DebuggerData::Snapshot(Snapshot {
            language: language.to_utf8_lossy(),
            id: Cow::Owned(generate_new_id().as_hyphenated().to_string()),
            probe: Some(probe.into()),
            timestamp,
            evaluation_errors: *errors,
            ..Default::default()
        }),
    })
}

pub fn serialize_debugger_payload(payload: &DebuggerPayload) -> String {
    serde_json::to_string(payload).unwrap()
}

#[no_mangle]
pub extern "C" fn ddog_serialize_debugger_payload(
    payload: &DebuggerPayload,
    callback: extern "C" fn(CharSlice),
) {
    let payload = serialize_debugger_payload(payload);
    callback(CharSlice::from(payload.as_str()))
}

#[no_mangle]
pub extern "C" fn ddog_drop_debugger_payload(_: Box<DebuggerPayload>) {}

pub fn ddog_debugger_diagnostics_create_unboxed<'a>(probe: &'a Probe, service: Cow<'a, str>, runtime_id: Cow<'a, str>, timestamp: u64) -> DebuggerPayload<'a> {
    let mut diagnostics = Diagnostics {
        probe_id: probe.id.to_utf8_lossy(),
        version: probe.version,
        status: probe.status,
        runtime_id,
        ..Default::default()
    };
    match probe.status {
        ProbeStatus::Error => {
            diagnostics.exception = Some(DiagnosticsError {
                r#type: probe.status_exception.to_utf8_lossy(),
                message: probe.status_msg.to_utf8_lossy(),
                stacktrace: if probe.status_exception.len() > 0 {
                    Some(probe.status_exception.to_utf8_lossy())
                } else {
                    None
                },
            });
        },
        ProbeStatus::Warning => diagnostics.details = Some(probe.status_msg.to_utf8_lossy()),
        _ => {},
    }
    DebuggerPayload {
        service,
        source: "dd_debugger",
        timestamp,
        message: Some(if probe.diagnostic_msg.len() > 0 {
            probe.diagnostic_msg.to_utf8_lossy()
        } else {
            Cow::Owned(match probe.status {
                ProbeStatus::Received => format!("Received definition for probe {}", &diagnostics.probe_id),
                ProbeStatus::Installed | ProbeStatus::Emitting => format!("Instrumented probe {}", &diagnostics.probe_id),
                ProbeStatus::Blocked => format!("Instrumentation denied for probe {}", &diagnostics.probe_id),
                ProbeStatus::Error => format!("Encountered error while instrumenting probe {}: {}", &diagnostics.probe_id, diagnostics.exception.as_ref().unwrap().message),
                ProbeStatus::Warning => format!("Probe {} warning: {}", &diagnostics.probe_id, diagnostics.details.as_ref().unwrap()),
            })
        }),
        debugger: DebuggerData::Diagnostics(diagnostics),
    }
}

#[no_mangle]
pub extern "C" fn ddog_debugger_diagnostics_create<'a>(probe: &'a Probe, service: CharSlice<'a>, runtime_id: CharSlice<'a>, timestamp: u64) -> Box<DebuggerPayload<'a>> {
    Box::new(ddog_debugger_diagnostics_create_unboxed(probe, service.to_utf8_lossy(), runtime_id.to_utf8_lossy(), timestamp))
}

#[no_mangle]
pub extern "C" fn ddog_debugger_diagnostics_set_parent_id<'a>(payload: &mut DebuggerPayload<'a>, parent_id: CharSlice<'a>) {
    let DebuggerData::Diagnostics(ref mut diagnostics) = payload.debugger else { unreachable!(); };
    diagnostics.parent_id = Some(parent_id.to_utf8_lossy());
}
