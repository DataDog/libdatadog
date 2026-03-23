// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

fn main() {
    // Only compile the TLS shim on Linux; the thread-level context feature is Linux-only.
    #[cfg(target_os = "linux")]
    {
        let mut build = cc::Build::new();

        // - On aarch64, TLSDESC is already the only dynamic TLS model so no flag is needed.
        // - On x86-64, we use `-mtls-dialect=gnu2` (supported since GCC 4.4 and Clang 19+) to force
        //   the use of TLSDESC as mandated by the spec. If it's not supported, this build will
        //   fail.
        #[cfg(target_arch = "x86_64")]
        build.flag("-mtls-dialect=gnu2");

        build.file("src/tls_shim.c").compile("tls_shim");
        println!("cargo:rerun-if-changed=src/tls_shim.c");
    }
}
