#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

# deploys and tests a cloud function with the given runtime version at the given path
# example:
# serverless/integration-tests/test-cloud-functions.sh nodejs18 serverless/integration-tests/cloud-function-node

# only works if ran in the repo root.

set -e

if [ "$#" -ne 3 ]; then
    echo "Please pass in three arguments: runtime version, path to the cloud function project, name of function handler."
fi

RUNTIME_VERSION="$1"
PROJECT_PATH="$2"
HANDLER_NAME="$3"

echo "RUNTIME_VERSION: $RUNTIME_VERSION"
echo "PROJECT_PATH: $PROJECT_PATH"

STAGE=$(xxd -l 4 -c 4 -p </dev/random)

FUNCTION_NAME=sls-mini-agent-integration-test-${RUNTIME_VERSION}-${STAGE}
echo "FUNCTION_NAME: $FUNCTION_NAME"

function cleanup {
    gcloud functions delete ${FUNCTION_NAME} --region us-east1 --gen2 --quiet 
}
trap cleanup EXIT

echo "Deploying integration test cloud function"

gcloud functions deploy ${FUNCTION_NAME} \
    --gen2 \
    --runtime "${RUNTIME_VERSION}" \
    --region us-east1 \
    --source "${PROJECT_PATH}" \
    --entry-point "${HANDLER_NAME}" \
    --trigger-http \
    --allow-unauthenticated \
    --env-vars-file "${PROJECT_PATH}/.env.yaml"

FUNCTION_URL=https://us-east1-datadog-sandbox.cloudfunctions.net/${FUNCTION_NAME}

echo "Invoking function"
curl -s -o /dev/null https://us-east1-datadog-sandbox.cloudfunctions.net/${FUNCTION_NAME}

echo "Waiting 1 minute before tailing logs"
sleep 60

LOGS=$(gcloud functions logs read ${FUNCTION_NAME} --region us-east1 --gen2 --limit 1000)

echo "$LOGS"

if echo "$LOGS" | grep -q "Successfully buffered traces to be flushed"; then
    echo "Mini Agent received traces"
    exit 0
else
    echo "Mini Agent DID NOT receive traces"
    exit 1
fi