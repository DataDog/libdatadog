// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

fn main() {
    // Only compile the TLS shim on Linux; the thread-level context feature is Linux-only.
    #[cfg(target_os = "linux")]
    {
        cc::Build::new().file("src/tls_shim.c").compile("tls_shim");
        println!("cargo:rerun-if-changed=src/tls_shim.c");
    }
}
