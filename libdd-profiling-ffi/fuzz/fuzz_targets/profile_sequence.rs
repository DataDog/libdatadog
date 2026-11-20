// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Valid profiling API sequences (new profile, add, metadata, serialize, send) through the C
//! symbols, allocation failure disabled. Inputs that reach owned branches no harness scenario
//! reaches become named scenarios of the allocation-failure harness
//! (`tests/allocation_failure/discovery/discovery.py promote`).

#![no_main]

use datadog_profiling_ffi as _;
use libfuzzer_sys::fuzz_target;

#[path = "../../tests/allocation_failure/agent.rs"]
mod agent;
#[path = "../../tests/allocation_failure/ffi.rs"]
mod ffi;
#[path = "../../tests/allocation_failure/sequence.rs"]
#[allow(dead_code)]
mod sequence;

fuzz_target!(|data: &[u8]| sequence::fuzz_one(data));
