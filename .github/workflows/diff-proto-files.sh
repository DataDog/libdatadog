#!/usr/bin/env bash

# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -eux

PROTO_FILE=""
DATADOG_AGENT_TAG="main"

while [[ $# -gt 0 ]]; do
  case $1 in
    --file)
      PROTO_FILE=$2
      shift && shift # past argument and value
      ;;
    --tag)
      DATADOG_AGENT_TAG=$2
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

GO_AGENT_PROTO=$(curl -s "https://raw.githubusercontent.com/DataDog/datadog-agent/$DATADOG_AGENT_TAG/pkg/proto/datadog/trace/$PROTO_FILE")
FIX_IMPORT_PATH=$(echo "$GO_AGENT_PROTO" | sed -e 's/import "datadog\/trace\//import "/g')
FIX_PACKAGE_NAME=$(echo "$FIX_IMPORT_PATH" | sed -e 's/datadog\.trace/pb/g')
echo "$FIX_PACKAGE_NAME" | diff -u "$PROTO_FILE" -
