#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -eu

fname=$1
ftmp="${fname}.tmp"
# preserve file attributes
cp -p "${fname}" "${ftmp}"
# process the file
cat "${fname}" | sed 's/^\([^a-zA-Z]*\).*Datadog.*Copyright \([0-9]*\)-Present.*$/\1Copyright \2-Present Datadog, Inc\. https\:\/\/www\.datadoghq\.com\/\n\1SPDX-License-Identifier: Apache-2.0/' | sed '/^\([^a-zA-Z]*\)Unless explicitly stated/d' | sed '/^\([^a-zA-Z]*\)under the Apache/d' | sed '/^\([^a-zA-Z]*\)Datadog, Inc.$/d' >| "${ftmp}" && mv "${ftmp}" "${fname}"
