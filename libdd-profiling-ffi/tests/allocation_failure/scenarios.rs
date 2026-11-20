// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Scenarios: a fixture built outside the scope, and one call run inside it.

use std::any::Any;
use std::hint::black_box;

#[derive(Debug)]
pub enum ScenarioError {
    /// The call reported an allocation failure.
    Oom,
    /// The call rejected its input. A setup bug, not a finding.
    Rejected,
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

pub const TAG_PUSH_EMPTY: &str = "tag_push_empty";
pub const TAG_PUSH_AT_CAPACITY: &str = "tag_push_at_capacity";
pub const TAG_PUSH_LONG: &str = "tag_push_long";

/// Bytes in the key and in the value of [`TAG_PUSH_LONG`].
const LONG_LEN: usize = 4096;

struct TagPush {
    tags: libdd_common_ffi::Vec<libdd_common::tag::Tag>,
    key: String,
    value: String,
}

fn tag_push(
    key: &str,
    value: &str,
    tags: libdd_common_ffi::Vec<libdd_common::tag::Tag>,
) -> TagPush {
    TagPush {
        tags,
        key: key.to_string(),
        value: value.to_string(),
    }
}

/// Pushes short tags until `len == capacity`, so the next push grows the buffer.
fn fill_to_capacity() -> libdd_common_ffi::Vec<libdd_common::tag::Tag> {
    let mut tags = libdd_common_ffi::tags::ddog_Vec_Tag_new();
    for n in 0..64 {
        let std_vec = tags.into_std();
        let full = !std_vec.is_empty() && std_vec.len() == std_vec.capacity();
        tags = std_vec.into();
        if full {
            return tags;
        }
        let key = format!("k{n}");
        let value = format!("v{n}");
        // SAFETY: `tags` comes from this API, and both slices come from live Rust strings.
        let result = unsafe {
            libdd_common_ffi::tags::ddog_Vec_Tag_push(
                &mut tags,
                libdd_common_ffi::CharSlice::from(key.as_str()),
                libdd_common_ffi::CharSlice::from(value.as_str()),
            )
        };
        assert!(
            matches!(result, libdd_common_ffi::tags::PushTagResult::Ok),
            "setup push {n} was rejected"
        );
    }
    panic!("the tag vector never reached capacity");
}

fn run_tag_push(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fixture = fixture.downcast_mut::<TagPush>().unwrap();
    // SAFETY: `tags` comes from this API, and both slices come from live Rust strings.
    let result = unsafe {
        libdd_common_ffi::tags::ddog_Vec_Tag_push(
            &mut fixture.tags,
            libdd_common_ffi::CharSlice::from(fixture.key.as_str()),
            libdd_common_ffi::CharSlice::from(fixture.value.as_str()),
        )
    };
    if matches!(result, libdd_common_ffi::tags::PushTagResult::Ok) {
        Ok(())
    } else {
        Err(ScenarioError::Rejected)
    }
}

pub fn find(name: &str) -> Option<&'static Scenario> {
    SCENARIOS
        .iter()
        .chain(TAG_SCENARIOS)
        .find(|s| s.name == name)
}

const TAG_SCENARIOS: &[Scenario] = &[
    Scenario {
        name: TAG_PUSH_EMPTY,
        setup: || {
            Box::new(tag_push(
                "env",
                "prod",
                libdd_common_ffi::tags::ddog_Vec_Tag_new(),
            ))
        },
        run: run_tag_push,
    },
    Scenario {
        name: TAG_PUSH_AT_CAPACITY,
        setup: || Box::new(tag_push("env", "prod", fill_to_capacity())),
        run: run_tag_push,
    },
    Scenario {
        name: TAG_PUSH_LONG,
        setup: || {
            Box::new(TagPush {
                tags: libdd_common_ffi::tags::ddog_Vec_Tag_new(),
                key: "k".repeat(LONG_LEN),
                value: "v".repeat(LONG_LEN),
            })
        },
        run: run_tag_push,
    },
];
