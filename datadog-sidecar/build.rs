// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    println!("cargo:rerun-if-env-changed=DATADOG_SIDECAR_APPSEC_PROTOCOL");
    // The file itself is tracked by rustc through include!().
    if std::env::var_os("CARGO_FEATURE_APPSEC_PROTOCOL").is_some()
        && std::env::var_os("DATADOG_SIDECAR_APPSEC_PROTOCOL").is_some()
    {
        println!("cargo:rustc-cfg=appsec_protocol_external");
    }
}
