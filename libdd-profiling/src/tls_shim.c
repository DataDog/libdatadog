// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Declares the thread-local pointer that external readers (e.g. the eBPF
// profiler) discover via the dynsym table. The Rust layer accesses this
// pointer in otel_thread_ctx.rs.
//
// The variable is declared in C in order to use the TLSDESC dialect for
// thread-local storage, which is required by the OTel thread-level context
// sharing spec. Unfortunately, it's not possible to have Rust use this dialect
// as of today.
#include <stddef.h>

__attribute__((visibility("default")))
__thread void *otel_thread_ctx_v1 = NULL;

// Return the resolved address of the thread-local variable.
void **libdd_get_otel_thread_ctx_v1(void) {
    return &otel_thread_ctx_v1;
}
