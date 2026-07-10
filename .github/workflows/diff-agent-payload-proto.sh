#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -eux

PROTO_FILE=""
AGENT_PAYLOAD_COMMIT=""

while [[ $# -gt 0 ]]; do
  case $1 in
    --file)
      PROTO_FILE=$2
      shift && shift # past argument and value
      ;;
    --commit)
      AGENT_PAYLOAD_COMMIT=$2
      shift && shift # past argument and value
      ;;
    *)
      echo "Unknown option $1"
      exit 1
      ;;
  esac
done

if [ -z "$PROTO_FILE" ]; then
  echo "Missing --file argument"
  exit 1
fi

if [ -z "$AGENT_PAYLOAD_COMMIT" ]; then
  echo "Missing --commit argument"
  exit 1
fi

# Vendored files must stay byte-for-byte identical to their pinned commit in agent-payload from
# `syntax = ...;` onward, so unlike diff-proto-files.sh (which fixes up import/package names for
# datadog-agent's proto layout) this only strips the local "Vendored from:" preamble comment
# before diffing, with no other rewriting.
curl -sf "https://raw.githubusercontent.com/DataDog/agent-payload/$AGENT_PAYLOAD_COMMIT/proto/metrics/$PROTO_FILE" |
  diff -u <(sed -n '/^syntax = /,$p' "$PROTO_FILE") -
