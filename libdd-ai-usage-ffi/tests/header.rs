// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The checked-in C header is what cbindgen generates from the crate. Run
//! with `UPDATE_HEADER=1` to regenerate it.

use std::fs;
use std::path::Path;

#[test]
fn header_is_current() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml")).unwrap();
    let mut generated = Vec::new();
    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .unwrap()
        .write(&mut generated);
    let generated = String::from_utf8(generated).unwrap();
    let path = crate_dir.join("include/ai_usage.h");
    if std::env::var_os("UPDATE_HEADER").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &generated).unwrap();
    }
    let checked_in = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        checked_in == generated,
        "include/ai_usage.h is out of date; run \
         `UPDATE_HEADER=1 cargo test -p libdd-ai-usage-ffi --test header`"
    );
}
