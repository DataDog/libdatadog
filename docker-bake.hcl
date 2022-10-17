target "alpine-base" {
  dockerfile = "tools/docker/Dockerfile.build"
  tags = ["ghcr.io/datadog/libdatadog:alpine-base"]
  target = "alpine_builder"
}

target "alpine-build" {
  dockerfile = "tools/docker/Dockerfile.build"
  args = {
    BUILDER_IMAGE = "alpine_builder"
  }
  target = "ffi_build_output"
  output = ["build/alpine_x64"]
}
