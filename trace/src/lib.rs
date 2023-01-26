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