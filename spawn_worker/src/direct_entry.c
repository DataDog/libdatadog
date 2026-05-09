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
#ifdef __linux__
# include <elf.h>
# include <link.h>
#endif

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

// Called by dl_iterate_phdr to run DT_INIT_ARRAY for our own library.
// Marked no_sanitize so it can run before ASAN's per-object init completes.
#ifdef __linux__
static int __attribute__((no_sanitize("address"), no_sanitize("undefined")))
run_init_array_cb(struct dl_phdr_info *info, size_t size, void *self_addr) {
    for (int i = 0; i < info->dlpi_phnum; i++) {
        if (info->dlpi_phdr[i].p_type != PT_LOAD) continue;
        uintptr_t start = info->dlpi_addr + info->dlpi_phdr[i].p_vaddr;
        uintptr_t end   = start + info->dlpi_phdr[i].p_memsz;
        if ((uintptr_t)self_addr < start || (uintptr_t)self_addr >= end) continue;
        // Found our library — locate DT_INIT_ARRAY in its DYNAMIC segment.
        for (int j = 0; j < info->dlpi_phnum; j++) {
            if (info->dlpi_phdr[j].p_type != PT_DYNAMIC) continue;
            ElfW(Dyn) *dyn = (ElfW(Dyn) *)(info->dlpi_addr + info->dlpi_phdr[j].p_vaddr);
            void (**arr)(void) = NULL;
            size_t sz = 0;
            for (; dyn->d_tag != DT_NULL; dyn++) {
                if (dyn->d_tag == DT_INIT_ARRAY)
                    arr = (void (**)(void))(info->dlpi_addr + dyn->d_un.d_ptr);
                if (dyn->d_tag == DT_INIT_ARRAYSZ)
                    sz = dyn->d_un.d_val;
            }
            if (arr) {
                for (size_t k = 0; k < sz / sizeof(void *); k++) {
                    if (arr[k] && (uintptr_t)arr[k] != (uintptr_t)-1)
                        arr[k]();
                }
            }
            return 1;
        }
    }
    return 0;
}
#endif /* __linux__ */

// Called by ld.so when the library is exec'd directly.
// Linked as the ELF e_entry.
//
// _DD_SIDECAR_DIRECT_EXEC must be set to the name of the symbol to call
__attribute__((visibility("default")))
void ddog_sidecar_direct_entry(void) {
    // Run our own DT_INIT_ARRAY before any other code.
    // ld.so skips DT_INIT_ARRAY for the main module in direct-exec mode, so
    // ASAN's per-object global registration and other constructors never run
    // unless we trigger them explicitly.
#ifdef __linux__
    dl_iterate_phdr(run_init_array_cb, (void *)&ddog_sidecar_direct_entry);
#endif

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
