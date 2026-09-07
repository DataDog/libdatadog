// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Minimal shared library for bin_tests to exercise the crashtracker's sigaction
// GOT hook.  Because this compiles as a separate .so, its PLT/GOT entry for
// sigaction is patched by the crashtracker's hook

#define _GNU_SOURCE
#include <signal.h>
#include <string.h>

// No-op SIGSEGV handler installed by dd_test_sigaction_on_sigsegv.
// Using a real function (not SIG_DFL / SIG_IGN) ensures the hook's
// handler_addr > 1 check passes and telemetry is emitted.
static void noop_sigsegv_handler(int signum) { (void)signum; }

// Installs noop_sigsegv_handler for SIGSEGV and stores the previous handler
// in *old_act.  The sigaction() call here goes through this library's
// PLT/GOT, which is patched by the crashtracker's hook.
// Returns 0 on success, -1 on error (errno set).
int dd_test_sigaction_on_sigsegv(struct sigaction *old_act) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = noop_sigsegv_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    return sigaction(SIGSEGV, &sa, old_act);
}
