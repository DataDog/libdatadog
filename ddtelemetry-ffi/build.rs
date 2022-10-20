// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{env, path::Path};

use cbindgen::{Config, Profile};
use rustc_version::Channel;

fn main() {
    match rustc_version::version_meta().unwrap().channel {
        Channel::Nightly => generate_header(true),
        _ => {
            generate_header(false);
        }
    }
}

fn generate_header(enable_expand: bool) {
    if env::var("_DD_TELEMETRY_RECURSION_GUARD").is_ok() {
        return;
    }
    std::env::set_var("_DD_TELEMETRY_RECURSION_GUARD", "");
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_dir = Path::new(&out_dir);

    let mut cfg = Config::from_file("cbindgen.toml").unwrap();

    if enable_expand {
        cfg.parse.expand.crates = vec!["ddtelemetry-ffi".into()];
        cfg.parse.expand.all_features = true;
        cfg.parse.expand.default_features = true;
        cfg.parse.expand.features = None;
        cfg.parse.expand.profile = Profile::Debug;
    }

    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(cfg)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(out_dir.join("ddtelemetry.h"));
}
