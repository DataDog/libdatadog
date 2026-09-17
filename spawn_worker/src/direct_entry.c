// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// This file provides the ELF entry point (ddog_spawn_direct_entry) for the
// shared library that contains spawn_worker.  When ld.so exec's that library
// directly, it calls this function rather than the trampoline.
// _DD_SIDECAR_DIRECT_EXEC must be set to the name of the symbol to call

#define _GNU_SOURCE
#include <alloca.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dlfcn.h>
#include <elf.h>
#include <link.h>

static inline FILE *error_fd() {
    char *log_env = getenv("DD_TRACE_LOG_FILE");
    if (log_env) {
        FILE *file = fopen(log_env, "a");
        if (file) {
            return file;
        }
    }
    return stderr;
}

// All fields are zero here (no deps to clean up).
typedef struct {
    int argc;
    const char **argv;
    const char **dependency_paths;
} trampoline_data_t;

// dlopen() each colon-separated path in _DD_SIDECAR_PATH_DEPS.
static void dlopen_path_deps(void) {
    const char *deps = getenv("_DD_SIDECAR_PATH_DEPS");
    if (!deps || !*deps) {
        return;
    }

    // Work on a copy so we can NUL-terminate each token in place.
    size_t len = strlen(deps);
    char *buf = alloca(len + 1);
    memcpy(buf, deps, len + 1);

    char *path = buf;
    while (*path) {
        char *colon = strchr(path, ':');
        if (colon) {
            *colon = '\0';
        }
        if (*path) {
            if (!dlopen(path, RTLD_LAZY | RTLD_GLOBAL)) {
                fputs(dlerror(), error_fd());
                _exit(11);
            }
        }
        if (!colon) {
            break;
        }
        path = colon + 1;
    }
}

// Called by dl_iterate_phdr to run DT_INIT_ARRAY for our own library.
// Returns 1 when our library is found and processed (stopping iteration),
// 0 otherwise.
// Marked no_sanitize so it can run before ASAN's per-object init completes.
static int __attribute__((no_sanitize("address"), no_sanitize("undefined")))
run_init_array_cb(struct dl_phdr_info *info, size_t size, void *self_addr) {
    (void)size;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        if (info->dlpi_phdr[i].p_type != PT_LOAD) {
            continue;
        }

        uintptr_t start = info->dlpi_addr + info->dlpi_phdr[i].p_vaddr;
        uintptr_t end = start + info->dlpi_phdr[i].p_memsz;
        if ((uintptr_t)self_addr < start || (uintptr_t)self_addr >= end) {
            continue;
        }

        // Found our library — locate DT_INIT_ARRAY in its DYNAMIC segment.
        for (int j = 0; j < info->dlpi_phnum; j++) {
            if (info->dlpi_phdr[j].p_type == PT_DYNAMIC) {
                void (**arr)(void) = NULL;
                size_t sz = 0;
                for (ElfW(Dyn) *dyn = (ElfW(Dyn) *)(info->dlpi_addr + info->dlpi_phdr[j].p_vaddr); dyn->d_tag != DT_NULL; dyn++) {
                    if (dyn->d_tag == DT_INIT_ARRAY) {
                        arr = (void (**)(void))(info->dlpi_addr + dyn->d_un.d_ptr);
                    }
                    if (dyn->d_tag == DT_INIT_ARRAYSZ) {
                        sz = dyn->d_un.d_val;
                    }
                }
                if (arr) {
                    typedef void (*init_fn_t)(int, char **, char **);
                    extern char **environ;
                    for (size_t k = 0; k < sz / sizeof(void *); k++) {
                        if (arr[k] && (uintptr_t)arr[k] != (uintptr_t)-1) {
                            ((init_fn_t)arr[k])(0, NULL, environ);
                        }
                    }
                }
                return 1;
            }
        }
    }
    return 0;
}

// Hidden (not static) so asm can reference it by name.
// @PLT in the call generates R_X86_64_PLT32 which old linkers accept for shared objects.
// 'used' prevents LTO from dropping it (it's only called from the naked asm below).
__attribute__((visibility("hidden"), used, noinline))
void ddog_spawn_direct_entry_body(void);

// Naked wrapper: ld.so JUMPs (not calls) to e_entry, so stack pointer alignment is
// unpredictable. We must align the stack BEFORE the C prologue runs: doing
// it inside the function body is too late because the prologue will already alter it.
__attribute__((visibility("default")))
#if defined(__x86_64__)
__attribute__((naked))
void ddog_spawn_direct_entry(void) {
    __asm__ (
        ".cfi_undefined rip\n\t" /* no valid return address: bottom of stack */
        "and $-16, %rsp\n\t"
        "call ddog_spawn_direct_entry_body@PLT\n\t"
        "ud2" /* unreachable: body calls _exit */
    );
}
#elif defined(__aarch64__)
__attribute__((naked))
void ddog_spawn_direct_entry(void) {
    __asm__ (
        ".cfi_undefined x30\n\t" /* no valid return address: bottom of stack */
        "mov x9, sp\n\t"
        "and x9, x9, #~15\n\t"
        "mov sp, x9\n\t"
        "bl ddog_spawn_direct_entry_body\n\t"
        "brk #0" /* unreachable: body calls _exit */
    );
}
#else
void ddog_spawn_direct_entry(void) {
    ddog_spawn_direct_entry_body();
}
#endif

__attribute__((visibility("hidden"), used, noinline))
void ddog_spawn_direct_entry_body(void) {
    // Run our own DT_INIT_ARRAY before any other code.
    // ld.so skips DT_INIT_ARRAY for the main module in direct-exec mode, so
    // ASAN's per-object global registration and other constructors never run
    // unless we trigger them explicitly.
    dl_iterate_phdr(run_init_array_cb, (void *)&ddog_spawn_direct_entry);

    const char *symbol_name = getenv("_DD_SIDECAR_DIRECT_EXEC");
    if (!symbol_name || !*symbol_name) {
        fputs("_DD_SIDECAR_DIRECT_EXEC is not set. Aborting.", error_fd());
        _exit(2);
    }

    // Load any path-dep libraries listed in _DD_SIDECAR_PATH_DEPS.
    dlopen_path_deps();

    // Call the requested symbol — avoids a link-time dependency on
    // datadog-sidecar from spawn_worker.
    typedef void (*entry_fn_t)(const trampoline_data_t *);
    entry_fn_t entry = (entry_fn_t)dlsym(RTLD_DEFAULT, symbol_name);
    if (entry) {
        trampoline_data_t data = {0};
        entry(&data);
    } else {
        fprintf(error_fd(), "fn was not found; missing %s in binary", symbol_name);
        _exit(12);
    }
    _exit(0);
}
