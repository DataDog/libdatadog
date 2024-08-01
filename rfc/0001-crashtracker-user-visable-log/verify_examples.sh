#!/usr/bin/env nix-shell
#!nix-shell -i bash -p yajsv

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0'

# this script uses nix provided yajsv to verify the examples using provided schema
# 
# install nix locally to run this script (e.g. via https://github.com/DeterminateSystems/nix-installer)

ROOT=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

cd ${ROOT}
yajsv -s schema.json "valid-*.json"
