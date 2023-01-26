// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

#![deny(clippy::all)]

pub mod pb {
    include!("./pb.rs");
}

mod normalize_utils_tests {
    include!("./normalization/tests/normalize_utils_tests.rs");
}

pub mod normalizer {
    include!("./normalization/normalizer.rs");
}

pub mod normalize_utils {
    include!("./normalization/normalize_utils.rs");
}

pub mod errors {
    include!("./normalization/errors.rs");
}
