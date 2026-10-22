// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <unistd.h>

static int writes;

int *fixture_data(void) {
    return &writes;
}

pid_t fixture_call(void) {
    ++writes;
    return getpid();
}

__attribute__((destructor)) static void finish(void) {
    ++writes;
}
