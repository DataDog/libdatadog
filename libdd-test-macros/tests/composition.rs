// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Each marked test here must be listed under its plain name by
//! `cargo nextest list` and under its `miri_` name by `cargo miri nextest list`.

use libdd_test_macros::miri_test;

#[miri_test]
fn bare() {}

#[miri_test]
#[test]
fn with_test_attr() {}

#[miri_test]
#[test_case::test_case(1 ; "one")]
#[test_case::test_case(2 ; "two")]
fn with_test_case(x: u8) {
    assert!(x > 0);
}

#[miri_test]
#[tokio::test(flavor = "current_thread")]
async fn with_tokio() {}

#[miri_test]
#[test]
#[should_panic(expected = "boom")]
fn with_should_panic() {
    panic!("boom");
}

#[miri_test]
mod module {
    #[test]
    fn inside() {}

    #[test]
    #[cfg_attr(miri, ignore = "opt-outs inside a marked module still apply")]
    fn opted_out() {}
}

#[cfg(not(miri))]
#[test]
fn plain_names_are_kept_outside_miri() {
    let _: fn() = bare;
}
