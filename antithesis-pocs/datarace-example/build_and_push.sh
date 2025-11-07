#!/usr/bin/env bash

set -euxo pipefail

description=${1:-"No description provided."}
version=${2:-vlatest}

base_url="us-central1-docker.pkg.dev/molten-verve-216720/datadog-repository/apm_ffi"

# Building & pushing config
config_url="$base_url/config:$version"
docker buildx build -t "$config_url" -f "Dockerfile.config" .
docker push "$config_url"

images_str=""
sep=""

# Building
for image in runner starter
do
  url="$base_url/$image:$version"
  docker buildx build -t "$url" -f "Dockerfile.$image" .
  docker push "$url"
  images_str="$images_str$sep$url"
  sep=";"
done

read -rp "Do you want to run? (y/N) " answer
if [[ "$answer" =~ ^[Yy]$ ]]
then
  curl --fail                                                                  \
    -u "datadog:$(cat datadog.password)"                                       \
    -X POST "https://datadog.antithesis.com/api/v1/launch/apm_ffi"             \
    -d "                                                                       \
    {                                                                          \
      \"params\":                                                              \
      {                                                                        \
        \"antithesis.description\": \"$description\",                          \
        \"antithesis.duration\":\"30\",                                        \
        \"antithesis.config_image\":\"$config_url\",                           \
        \"antithesis.images\":\"$images_str\",                                 \
        \"antithesis.report.recipients\":\"jules.wiriath@datadoghq.com\"       \
      }                                                                        \
    }"
fi
