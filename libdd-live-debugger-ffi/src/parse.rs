// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::data::LiveDebuggingData;
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::CharSlice;

#[repr(C)]
pub struct LiveDebuggingParseResult {
    pub data: LiveDebuggingData<'static>,
    opaque_data: Option<Box<libdd_live_debugger::LiveDebuggingData>>,
}

/// # Safety
/// The `json` must be a valid UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_parse_live_debugger_json(
    json: CharSlice,
) -> LiveDebuggingParseResult {
    match libdd_live_debugger::parse_json(unsafe { json.assume_utf8() }) {
        Ok(parsed) => {
            let parsed = Box::new(parsed);
            LiveDebuggingParseResult {
                // we have the box. Rust doesn't allow us to specify a self-referential struct, so
                // pretend it's 'static
                data: unsafe {
                    std::mem::transmute::<&_, &'static libdd_live_debugger::LiveDebuggingData>(
                        &*parsed,
                    )
                }
                .into(),
                opaque_data: Some(parsed),
            }
        }
        _ => LiveDebuggingParseResult {
            data: LiveDebuggingData::None,
            opaque_data: None,
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_drop_live_debugger_parse_result(_: LiveDebuggingParseResult) {}
