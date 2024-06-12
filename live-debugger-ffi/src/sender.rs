use std::borrow::Cow;
use ddcommon_ffi::CharSlice;
// Alias to prevent cbindgen panic
use datadog_live_debugger::debugger_defs::{Value as DebuggerValueAlias, Capture as DebuggerCaptureAlias, Captures, DebuggerData, Entry, Fields, DebuggerPayload, Snapshot, SnapshotEvaluationError};
use datadog_live_debugger::sender::generate_new_id;
use ddcommon_ffi::slice::AsBytes;
use crate::data::Probe;

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
            value: if val.value.len() == 0 { None } else { Some(val.value.to_utf8_lossy()) },
            fields: if let Some(boxed) = val.fields { *boxed } else { Fields::default() },
            elements: unsafe { std::mem::transmute(val.elements) }, // SAFETY: is transparent
            entries: val.entries,
            is_null: val.is_null,
            truncated: val.truncated,
            not_captured_reason: if val.not_captured_reason.len() == 0 { None } else { Some(val.not_captured_reason.to_utf8_lossy()) },
            size: if val.size.len() == 0 { None } else { Some(val.size.to_utf8_lossy()) },
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
pub extern "C" fn ddog_create_exception_snapshot<'a>(buffer: &mut Vec<DebuggerPayload<'a>>, service: CharSlice<'a>, language: CharSlice<'a>, id: CharSlice<'a>, exception_id: CharSlice<'a>, timestamp: u64) -> *mut DebuggerCapture<'a> {
    let snapshot = DebuggerPayload {
        service: service.to_utf8_lossy(),
        source: "dd_debugger",
        timestamp,
        message: None,
        debugger: DebuggerData {
            snapshot: Snapshot {
                captures: Some(Captures {
                    r#return: Some(DebuggerCaptureAlias::default()),
                    ..Default::default()
                }),
                language: language.to_utf8_lossy(),
                id: id.to_utf8_lossy(),
                exception_id: Some(exception_id.to_utf8_lossy()),
                timestamp,
                ..Default::default()
            }
        }
    };
    buffer.push(snapshot);
    unsafe { std::mem::transmute(buffer.last_mut().unwrap().debugger.snapshot.captures.as_mut().unwrap().r#return.as_mut().unwrap()) }
}

pub extern "C" fn ddog_create_log_probe_snapshot<'a>(buffer: &mut Box<DebuggerPayload<'a>>, probe: &'a Probe, service: CharSlice<'a>, language: CharSlice<'a>, timestamp: u64) -> *mut DebuggerCapture<'a> {
    *buffer = Box::new(DebuggerPayload {
        service: service.to_utf8_lossy(),
        source: "dd_debugger",
        timestamp,
        message: None,
        debugger: DebuggerData {
            snapshot: Snapshot {
                captures: Some(Captures {
                    r#return: Some(DebuggerCaptureAlias::default()),
                    ..Default::default()
                }),
                language: language.to_utf8_lossy(),
                id: Cow::Owned(generate_new_id().as_hyphenated().to_string()),
                probe: Some(probe.into()),
                timestamp,
                ..Default::default()
            }
        }
    });
    unsafe { std::mem::transmute(buffer.debugger.snapshot.captures.as_mut().unwrap().r#return.as_mut().unwrap()) }
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_snapshot_add_field<'a, 'b: 'a, 'c: 'a>(capture: &mut DebuggerCapture<'a>, r#type: FieldType, name: CharSlice<'b>, value: CaptureValue<'c>) {
    let fields = match r#type {
        FieldType::STATIC => &mut capture.0.static_fields,
        FieldType::ARG => &mut capture.0.arguments,
        FieldType::LOCAL => &mut capture.0.locals,
    };
    fields.insert(name.to_utf8_lossy(), value.into());
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_capture_value_add_element<'a, 'b: 'a>(value: &mut CaptureValue<'a>, element: CaptureValue<'b>) {
    value.elements.push(DebuggerValue(element.into()));
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_capture_value_add_entry<'a, 'b: 'a, 'c: 'a>(value: &mut CaptureValue<'a>, key: CaptureValue<'b>, element: CaptureValue<'c>) {
    value.entries.push(Entry(key.into(), element.into()));
}

#[no_mangle]
#[allow(improper_ctypes_definitions)] // Vec has a fixed size, and we care only about that here
pub extern "C" fn ddog_capture_value_add_field<'a, 'b: 'a, 'c: 'a>(value: &mut CaptureValue<'a>, key: CharSlice<'b>, element: CaptureValue<'c>) {
    let fields = match value.fields {
        None => {
            value.fields = Some(Box::default());
            value.fields.as_mut().unwrap()
        },
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
pub extern "C" fn ddog_evaluation_error_drop(_: Box<Vec<SnapshotEvaluationError>>) {
}

#[no_mangle]
pub extern "C" fn ddog_evaluation_error_snapshot<'a>(probe: &'a Probe, service: CharSlice<'a>, language: CharSlice<'a>, errors: Box<Vec<SnapshotEvaluationError>>, timestamp: u64) -> Box<DebuggerPayload<'a>> {
    Box::new(DebuggerPayload {
        service: service.to_utf8_lossy(),
        source: "dd_debugger",
        timestamp,
        message: Some(format!("Evaluation errors for probe id {}", probe.id)),
        debugger: DebuggerData {
            snapshot: Snapshot {
                language: language.to_utf8_lossy(),
                id: Cow::Owned(generate_new_id().as_hyphenated().to_string()),
                probe: Some(probe.into()),
                timestamp,
                evaluation_errors: *errors,
                ..Default::default()
            }
        }
    })
}
