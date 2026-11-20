// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Scenarios: a fixture built outside the scope, and one call run inside it.

use std::any::Any;
use std::hint::black_box;

#[derive(Debug)]
pub enum ScenarioError {
    /// The call reported an allocation failure.
    Oom,
}

pub struct Scenario {
    pub name: &'static str,
    pub setup: fn() -> Box<dyn Any>,
    pub run: fn(&mut Box<dyn Any>) -> Result<(), ScenarioError>,
}

fn no_fixture() -> Box<dyn Any> {
    Box::new(())
}

/// Toy scenarios: plain Rust allocations, no libdatadog code. They check the harness itself.
pub const TOY_BOX_NEW: &str = "toy_box_new";
pub const TOY_VEC_PUSH: &str = "toy_vec_push";
pub const TOY_VEC_TRY_RESERVE: &str = "toy_vec_try_reserve";
pub const TOY_STRING_FORMAT: &str = "toy_string_format";

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: TOY_BOX_NEW,
        setup: no_fixture,
        run: |_| {
            black_box(Box::new(black_box([7u8; 64])));
            Ok(())
        },
    },
    Scenario {
        name: TOY_VEC_PUSH,
        setup: no_fixture,
        run: |_| {
            let mut v = Vec::new();
            for i in 0..100u64 {
                v.push(black_box(i));
            }
            black_box(v);
            Ok(())
        },
    },
    Scenario {
        name: TOY_VEC_TRY_RESERVE,
        setup: no_fixture,
        run: |_| {
            let mut v: Vec<u8> = Vec::new();
            v.try_reserve(black_box(1024))
                .map_err(|_| ScenarioError::Oom)?;
            black_box(v);
            Ok(())
        },
    },
    Scenario {
        name: TOY_STRING_FORMAT,
        setup: no_fixture,
        run: |_| {
            black_box(format!("{}-{}", black_box(42), black_box("forty-two")));
            Ok(())
        },
    },
];

pub fn find(name: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.name == name)
}
