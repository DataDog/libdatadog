use crate::data::LiveDebuggingData;
use ddcommon_ffi::slice::AsBytes;
use ddcommon_ffi::CharSlice;

#[repr(C)]
pub struct LiveDebuggingParseResult {
    pub data: LiveDebuggingData<'static>,
    opaque_data: Option<Box<datadog_live_debugger::LiveDebuggingData>>,
}

#[no_mangle]
pub extern "C" fn ddog_parse_live_debugger_json(json: CharSlice) -> LiveDebuggingParseResult {
    if let Ok(parsed) =
        datadog_live_debugger::parse_json(unsafe { std::str::from_utf8_unchecked(json.as_bytes()) })
    {
        let parsed = Box::new(parsed);
        LiveDebuggingParseResult {
            // we have the box. Rust doesn't allow us to specify a self-referential struct, so pretend it's 'static
            data: unsafe {
                std::mem::transmute::<&_, &'static datadog_live_debugger::LiveDebuggingData>(
                    &*parsed,
                )
            }
            .into(),
            opaque_data: Some(parsed),
        }
    } else {
        LiveDebuggingParseResult {
            data: LiveDebuggingData::None,
            opaque_data: None,
        }
    }
}

#[no_mangle]
pub extern "C" fn ddog_drop_live_debugger_parse_result(_: LiveDebuggingParseResult) {}
