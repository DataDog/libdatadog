// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "wasi")]
#![deny(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use libdd_data_pipeline_core::{TracerMetadata, prepare_v04_payload};
use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
use libdd_trace_utils::msgpack_decoder::v04::from_slice;
use std::{collections::HashMap, ptr, sync::Mutex};

const MAX_INPUT_BYTES: u32 = 50 * 1024 * 1024;

struct Workspace {
    config: Option<ObfuscationConfig>,
    metadata: Option<TracerMetadata>,
    input: Vec<u8>,
    output: Vec<u8>,
}

static WORKSPACE: Mutex<Workspace> = Mutex::new(Workspace {
    config: None,
    metadata: None,
    input: Vec::new(),
    output: Vec::new(),
});

fn with_workspace<Output: Default>(
    apply: impl FnOnce(&mut Workspace) -> Output + std::panic::UnwindSafe,
) -> Output {
    std::panic::catch_unwind(|| match WORKSPACE.lock() {
        Ok(mut workspace) => apply(&mut workspace),
        Err(_) => Output::default(),
    })
    .unwrap_or_default()
}

#[unsafe(no_mangle)]
pub extern "C" fn input_buffer(length: u32) -> *mut u8 {
    with_workspace(|workspace| {
        workspace.input.clear();
        workspace.output = Vec::new();
        if length == 0 || length > MAX_INPUT_BYTES {
            return ptr::null_mut();
        }
        let length = length as usize;
        if workspace.input.try_reserve(length).is_err() {
            return ptr::null_mut();
        }
        workspace.input.resize(length, 0);
        workspace.input.as_mut_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn configure() -> u32 {
    with_workspace(|workspace| {
        workspace.config = serde_json::from_slice(&workspace.input).ok();
        workspace.input.clear();
        u32::from(workspace.config.is_some())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn configure_metadata(client_computed_top_level: u32) -> u32 {
    with_workspace(|workspace| {
        let fields = serde_json::from_slice::<HashMap<String, String>>(&workspace.input);
        workspace.input.clear();
        workspace.metadata = None;
        let Ok(mut fields) = fields else {
            return 0;
        };
        if client_computed_top_level > 1 {
            return 0;
        }
        let metadata = TracerMetadata {
            hostname: fields.remove("hostname").unwrap_or_default(),
            env: fields.remove("env").unwrap_or_default(),
            language: fields.remove("language").unwrap_or_default(),
            language_version: fields.remove("languageVersion").unwrap_or_default(),
            tracer_version: fields.remove("tracerVersion").unwrap_or_default(),
            client_computed_top_level: client_computed_top_level == 1,
            ..TracerMetadata::default()
        };
        metadata
            .mutable_metadata
            .set_runtime_id(fields.remove("runtimeID").unwrap_or_default());
        workspace.metadata = Some(metadata);
        1
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn prepare(client_side_stats: u32) -> u32 {
    with_workspace(|workspace| {
        workspace.output = Vec::new();
        let output = prepare_input(workspace, client_side_stats);
        workspace.input.clear();
        let Some(output) = output else {
            return 0;
        };
        let Ok(length) = u32::try_from(output.len()) else {
            return 0;
        };
        workspace.output = output;
        length
    })
}

fn prepare_input(workspace: &Workspace, client_side_stats: u32) -> Option<Vec<u8>> {
    if client_side_stats > 1 {
        return None;
    }
    let config = workspace.config.as_ref()?;
    let metadata = workspace.metadata.as_ref()?;
    let (mut traces, _) = from_slice(&workspace.input).ok()?;
    prepare_v04_payload(&mut traces, metadata, config, client_side_stats == 1).ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn output_buffer() -> *const u8 {
    with_workspace(|workspace| {
        if workspace.output.is_empty() {
            return ptr::null();
        }
        workspace.output.as_ptr()
    })
}
