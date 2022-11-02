// Checks

target "_check_base" {
  dockerfile = "tools/docker/checks.Dockerfile"
  output = ["type=cacheonly"]
}

target "check_license_headers" {
  inherits = ["_check_base", "_use_debian_stable"]
  target = "check_license_headers"
}

target "check_license_3rdparty_file" {
  inherits = ["_check_base", "_use_debian_stable"]
  target = "check_license_3rdparty_file"
}

target "check_rust_fmt" {
  inherits = ["_check_base", "_use_debian_nightly"]
  target = "check_rust_fmt"
}

target "check_clippy_stable" {
  inherits = ["_check_base", "_use_debian_stable"]
  target = "check_clippy"
}

target "check_clippy_nightly" {
  inherits = ["_check_base", "_use_debian_nightly"]
  target = "check_clippy"
}

target "check_clippy_1_60" {
  inherits = ["_check_base", "_use_debian_1_60"]
  target = "check_clippy"
}

group "check_clippy" {
  targets = ["check_clippy_stable", "check_clippy_nightly", "check_clippy_1_60", ]
}

group "checks" {
  targets = ["check_license_headers", "check_license_3rdparty_file", "check_rust_fmt", "check_clippy"]
}

// generate files
target "update_license_file" {
  inherits = ["_check_base", "_use_debian_stable"]
  target = "export_license_3rdparty_file"
  output = ["./"]
}

// cache
target "cargo_registry_cache" {
  dockerfile = "tools/docker/cargo.Dockerfile"
  output = ["type=image"]
}

// builders
target "alpine_builder_stable" {
  dockerfile = "tools/docker/alpine.Dockerfile"
  target = "builder"
  platforms = ["linux/amd64", "linux/arm64"]
  contexts = {
    cargo_registry_cache = "target:cargo_registry_cache"
  }
}

target "debian_builder_stable" {
  dockerfile = "tools/docker/debian.Dockerfile"
  target = "builder"
  platforms = ["linux/amd64", "linux/arm64"]
  args = {
    RUST_BASE_IMAGE = "rust:1-slim-bullseye"
  }
  contexts = {
    cargo_registry_cache = "target:cargo_registry_cache"
  }
}

target "debian_builder_nightly" {
  inherits = ["debian_builder_stable"]
  args = {
    RUST_BASE_IMAGE = "rustlang/rust:nightly-bullseye-slim"
  }
  platforms = ["linux/amd64", "linux/arm64"]
}

target "debian_builder_1_60" {
  inherits = ["debian_builder_stable"]
  args = {
    RUST_BASE_IMAGE = "rust:1.60-slim-bullseye"
  }
  platforms = ["local"]
}

group "all_builders" {
  targets = ["alpine_builder_stable", "debian_builder_stable", "debian_builder_nightly", "debian_builder_1_60"]
}

target "_use_debian_nightly" {
  contexts = {
    // base = "target:debian_builder_nightly"
    base = "docker-image://ghcr.io/datadog/libdatadog-ci:debian_builder_nightly"
  }
}

target "_use_debian_stable" {
  contexts = {
    // base = "target:debian_builder_stable"
    base = "docker-image://ghcr.io/datadog/libdatadog-ci:debian_builder_stable"
  }
}

target "_use_debian_1_60" {
  contexts = {
    // base = "target:debian_builder_1_60"
    base = "docker-image://ghcr.io/datadog/libdatadog-ci:debian_builder_1_60"
  }
}

// CI
group "build_ci_images" {
  targets = ["all_builders", "cargo_registry_cache"]
}