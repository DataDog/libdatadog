// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Named scenarios made from fuzz inputs (see `promoted.rs` and `discovery/discovery.py`).
//!
//! The setup decodes the input and runs it once with allocations working, to learn which calls
//! reject their input. The scope then runs the whole sequence; an error on a call that did not
//! fail in the setup run is the allocation failure. Ordinals come from this build: nothing is
//! taken from the coverage or fuzzing builds that found the input.

use crate::agent::local_agent;
use crate::scenarios::{Scenario, ScenarioError};
use crate::sequence::{self, Plan, Run};
use std::any::Any;

pub struct Promoted {
    pub scenario: Scenario,
    pub bytes: &'static [u8],
}

impl Promoted {
    /// The sequence sends: its trace depends on I/O timing.
    pub fn io_bound(&self) -> bool {
        Plan::decode(self.bytes).sends()
    }
}

/// `name, "description", [bytes];` entries become [`Promoted`] scenarios in `PROMOTED`.
macro_rules! promoted {
    ($($name:ident, $inputs:literal, [$($b:literal),* $(,)?];)*) => {
        pub const PROMOTED: &[$crate::fuzzed::Promoted] = &[$(
            $crate::fuzzed::Promoted {
                scenario: $crate::scenarios::Scenario {
                    name: stringify!($name),
                    inputs: $inputs,
                    setup: || $crate::fuzzed::setup(&[$($b),*]),
                    run: $crate::fuzzed::run,
                    check: Some($crate::fuzzed::check),
                },
                bytes: &[$($b),*],
            },
        )*];
    };
}
pub(crate) use promoted;

struct SequenceFixture {
    plan: Plan,
    run: Run,
    /// Calls that return an error with allocations working (rejected inputs).
    expected: Vec<bool>,
    errors: Vec<bool>,
    /// The post-failure probe on the setup run. Release builds skip sample label validation, so
    /// an invalid label is accepted and every later serialize fails, with or without failures.
    reference: Result<(), String>,
}

pub fn setup(bytes: &[u8]) -> Box<dyn Any> {
    let plan = Plan::decode(bytes);
    let agent = plan.sends().then(local_agent);
    let mut expected = plan.errors();
    let mut reference_run = Run::new(agent);
    sequence::execute(&plan, &mut reference_run, None, &mut expected)
        .expect("errors are ignored without expected");
    let reference = match reference_run.profile.as_mut() {
        Some(profile) => probe(profile, &plan),
        None => Err("no profile".to_string()),
    };
    Box::new(SequenceFixture {
        errors: plan.errors(),
        plan,
        run: Run::new(agent),
        expected,
        reference,
    })
}

pub fn run(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<SequenceFixture>().unwrap();
    sequence::execute(&fx.plan, &mut fx.run, Some(&fx.expected), &mut fx.errors)
        .map_err(|_| ScenarioError::Oom)
}

/// A failed `Profile_new` publishes no profile. Otherwise the profile takes a sample and
/// serializes, whatever call failed, when it does so in the setup run.
///
/// Contract: provisional. `profiling.h` documents neither the error variant of `Profile_new`
/// nor the profile state after a failed `add`, `serialize`, metadata call or send.
pub fn check(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<SequenceFixture>().unwrap();
    let Some(profile) = fx.run.profile.as_mut() else {
        return if failed {
            Ok(())
        } else {
            Err("success without a profile".to_string())
        };
    };
    if profile.inner.is_null() {
        return Err("the profile handle is null".to_string());
    }
    match (probe(profile, &fx.plan), &fx.reference) {
        (Err(e), Ok(())) => Err(format!("{e}, after the call returned an error")),
        _ => Ok(()),
    }
}

/// Adds the probe sample, then serializes.
fn probe(profile: &mut crate::ffi::Profile, plan: &Plan) -> Result<(), String> {
    if sequence::add(profile, &plan.probe) {
        return Err("add failed".to_string());
    }
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`.
    match unsafe { crate::ffi::ddog_prof_Profile_serialize(profile, None, None) } {
        crate::ffi::SerializeResult::Ok(mut encoded) => {
            // SAFETY: `encoded` comes from `ddog_prof_Profile_serialize`.
            unsafe { crate::ffi::ddog_prof_EncodedProfile_drop(&mut encoded) };
            Ok(())
        }
        crate::ffi::SerializeResult::Err(mut error) => {
            let message = format!("serialize failed: {error}");
            // SAFETY: `error` is a valid error returned by the FFI.
            unsafe { libdd_common_ffi::ddog_Error_drop(Some(&mut error)) };
            Err(message)
        }
    }
}
