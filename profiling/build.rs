// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use prost_build::Config;

fn main() -> Result<(), std::io::Error> {
    let mut config = Config::new();
    config
        .type_attribute("pprof.Function", "#[derive(Copy, Eq, Hash)]")
        .type_attribute("pprof.Label", "#[derive(Copy, Eq, Hash)]")
        .type_attribute("pprof.Line", "#[derive(Copy, Eq, Hash)]")
        .type_attribute("pprof.Location", "#[derive(Eq, Hash)]")
        .type_attribute("pprof.Mapping", "#[derive(Eq, Hash)]")
        .type_attribute("pprof.Profile", "#[derive(Eq, Hash)]")
        .type_attribute("pprof.Sample", "#[derive(Eq, Hash)]")
        .type_attribute("pprof.ValueType", "#[derive(Copy, Eq, Hash)]");

    let protos = &[concat!(env!("CARGO_MANIFEST_DIR"), "/src/profile.proto")];
    let includes = &[concat!(env!("CARGO_MANIFEST_DIR"), "/src")];
    config.compile_protos(protos, includes)?;
    Ok(())
}
