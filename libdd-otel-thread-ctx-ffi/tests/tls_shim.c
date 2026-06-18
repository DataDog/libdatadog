// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern __thread void *otel_thread_ctx_v1 __attribute__((tls_model("global-dynamic")));

__attribute__((noinline)) void **tls_slot_from_c(void) {
    return &otel_thread_ctx_v1;
}
