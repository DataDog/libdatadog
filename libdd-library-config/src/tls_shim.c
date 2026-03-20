// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

// Declares the thread-local pointer that external readers (e.g. the eBPF
// profiler) discover via the dynsym table. The Rust layer accesses this
// pointer in otel_thread_ctx.rs.
//
// The variable is be declared in C in order to use the TLSDESC dialect for
// thread-local storage, which is required by the OTel thread-level context
// sharing spec. Unfortunately, it's not possible to have Rust use this dialect
// as of today.
#include <stddef.h>

__attribute__((visibility("default")))
__thread void *custom_labels_current_set_v2 = NULL;

// Return the resolved address of the thread-local variable.
void **libdd_get_custom_labels_current_set_v2(void) {
    return &custom_labels_current_set_v2;
}
