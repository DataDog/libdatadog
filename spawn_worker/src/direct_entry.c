// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// This file provides the ELF entry point (ddog_sidecar_direct_entry) for the
// shared library that contains spawn_worker (ddtrace.so in non-SSI builds,
// libddtrace_php.so in SSI builds).  When ld.so exec's that library directly,
// it calls this function rather than the trampoline.
//
// Linked as the ELF e_entry via:
//  - cargo:rustc-cdylib-link-arg=-Wl,-e,ddog_sidecar_direct_entry (cdylib / SSI)
//  - -Wl,-e,ddog_sidecar_direct_entry in EXTRA_LDFLAGS (ddtrace.so / non-SSI)

#define _GNU_SOURCE
#include <alloca.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dlfcn.h>

// All fields are null/zero when calling from Direct spawn (no deps to clean up).
typedef struct {
    int argc;
    const char **argv;
    const char **dependency_paths;
} trampoline_data_t;

// dlopen() each colon-separated path in _DD_SIDECAR_PATH_DEPS.
static void dlopen_path_deps(void) {
    const char *deps = getenv("_DD_SIDECAR_PATH_DEPS");
    if (!deps || !*deps) return;

    // Work on a copy so we can NUL-terminate each token in place.
    size_t len = strlen(deps);
    char *buf = alloca(len + 1);
    memcpy(buf, deps, len + 1);

    char *p = buf;
    while (*p) {
        char *colon = strchr(p, ':');
        if (colon) *colon = '\0';
        if (*p) dlopen(p, RTLD_LAZY | RTLD_GLOBAL);
        if (!colon) break;
        p = colon + 1;
    }
}

// Called by ld.so when the library is exec'd directly.
// Linked as the ELF e_entry.
//
// _DD_SIDECAR_DIRECT_EXEC must be set to the name of the symbol to call
__attribute__((visibility("default")))
void ddog_sidecar_direct_entry(void) {
    const char *sym_name = getenv("_DD_SIDECAR_DIRECT_EXEC");
    if (!sym_name || !*sym_name) {
        _exit(1);
    }

    // Load any path-dep libraries listed in _DD_SIDECAR_PATH_DEPS.
    dlopen_path_deps();

    // Call the requested symbol — avoids a link-time dependency on
    // datadog-sidecar from spawn_worker.
    typedef void (*entry_fn_t)(const trampoline_data_t *);
    entry_fn_t entry = (entry_fn_t)dlsym(RTLD_DEFAULT, sym_name);
    if (entry) {
        trampoline_data_t data = {0};
        entry(&data);
    }
    _exit(0);
}
