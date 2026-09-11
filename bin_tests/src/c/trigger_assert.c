// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#undef NDEBUG
#include <assert.h>

void trigger_c_assert(void) {
    int test_value = 0;
    assert(test_value > 0);
}
