// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

fn main() {
    // ensure symbols are properly exported for dlsym to be able to look them up
    println!("cargo:rustc-link-arg-tests=-rdynamic")
}
