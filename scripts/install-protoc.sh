#! /usr/bin/env bash

# Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -o errexit -o verbose

# Protoc. No guard because we want to override Ubuntu's old version in
# case it is already installed by a dependency.
#
# Basis of script copied from:
# https://github.com/paxosglobal/asdf-protoc/blob/46c2f9349b8420144b197cfd064a9677d21cfb0c/bin/install

# shellcheck disable=SC2155
readonly TMP_DIR="$(mktemp -d -t "protoc_XXXX")"
trap 'rm -rf "${TMP_DIR?}"' EXIT

get_platform() {
  local os
  os=$(uname)
  if [[ "${os}" == "Darwin" ]]; then
    echo "osx"
  elif [[ "${os}" == "Linux" ]]; then
    echo "linux"
  elif [[ "${os}" == "MINGW"* || "${os}" == "MSYS"* ]]; then
    echo "win"
  else
    echo >&2 "unsupported os: ${os}" && exit 1
  fi
}

get_arch() {
  local os
  local arch
  os=$(uname)
  arch=$(uname -m)
  # On ARM Macs, uname -m returns "arm64", but in protoc releases this architecture is called "aarch_64"
  if [[ "${os}" == "Darwin" && "${arch}" == "arm64" ]]; then
    echo "-aarch_64"
  elif [[ "${os}" == "Linux" && "${arch}" == "aarch64" ]]; then
    echo "-aarch_64"
  elif [[ ("${os}" == "MINGW"* || "${os}" == "MSYS"*) && "${arch}" == "x86_64" ]]; then
    echo "64"
  elif [[ ("${os}" == "MINGW"* || "${os}" == "MSYS"*) && "${arch}" == "i686" ]]; then
    echo "32"
  else
    echo "-${arch}"
  fi
}

install_protoc() {
  local version=$1

  # For Github Actions, install_path is set to the HOME environment variable
  # For the Linux based Dockerfile, install_path is set to usr/local
  if [ -n "$GITHUB_PATH" ]; then
    install_path=$HOME
  else
    install_path="usr/local"
  fi

  mkdir -p "${install_path}"

  local base_url="https://github.com/protocolbuffers/protobuf/releases/download"
  local url
  url="${base_url}/v${version}/protoc-${version}-$(get_platform)$(get_arch).zip"
  local download_path="${TMP_DIR}/protoc.zip"

  echo "Downloading ${url}"
  curl -fsSL "${url}" -o "${download_path}"

  unzip -qq "${download_path}" -d "${install_path}"

  # Set PATH appropriately depending on where script is running
  if [ -n "$GITHUB_PATH" ]; then
    echo "${install_path}/bin" >> $GITHUB_PATH
  else
    export PATH="$PATH:${install_path}/bin"
  fi
}

install_protoc "28.0"
