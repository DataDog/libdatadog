target "alpine-base" {
  dockerfile = "tools/docker/build.Dockerfile"
  tags = ["ghcr.io/datadog/libdatadog-build:alpine-base"]
  target = "alpine_builder"
}

target "alpine-build" {
  dockerfile = "tools/docker/build.Dockerfile"
  args = {
    BUILDER_IMAGE = "alpine_builder"
  }
  target = "ffi_build_output"
  platforms = ["linux/amd64"]
  output = ["build/x86_64-alpine-linux-musl"]
}

target "debian-build" {
  dockerfile = "tools/docker/build.Dockerfile"
  args = {
    BUILDER_IMAGE = "debian_builder"
  }
  target = "ffi_build_output"
  platforms = ["linux/amd64"]
  output = ["build/x86_64-unknown-linux-gnu"]
}

target "alpine-build-aarch64" {
  inherits = ["alpine-build"]
  platforms = ["linux/arm64"]
  output = ["build/aarch64-alpine-linux-musl"]
}

target "debian-build-aarch64" {
  inherits = ["debian-build"]
  platforms = ["linux/arm64"]
  output = ["build/aarch64-unknown-linux-gnu"]
}

group "build" {
  targets = ["alpine-build", "debian-build"]
}