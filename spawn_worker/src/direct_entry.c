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
#include <fcntl.h>
#include <signal.h>
#ifdef __linux__
# include <sys/ucontext.h>
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dlfcn.h>
#ifdef __linux__
# include <elf.h>
# include <link.h>
#endif
/* HAVE_BACKTRACE is defined by build.rs when execinfo is available and links */
#ifdef HAVE_BACKTRACE
# include <execinfo.h>
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
                typedef void (*init_fn_t)(int, char **, char **);
                extern char **environ;
                for (size_t k = 0; k < sz / sizeof(void *); k++) {
                    if (arr[k] && (uintptr_t)arr[k] != (uintptr_t)-1)
                        ((init_fn_t)arr[k])(0, NULL, environ);
                }
            }
            return 1;
        }
    }
    return 0;
}
#endif

// Signal handler: write crash info to stderr AND /tmp/ddog_sidecar_crash_<pid>.
// Uses only async-signal-safe functions.
static void crash_handler(int sig, siginfo_t *si, void *ctx) {
    (void)si;
    char path[64];
    pid_t pid = getpid();
    const char prefix[] = "/tmp/ddog_sidecar_crash_";
    int pos = 0;
    for (int i = 0; prefix[i]; i++) path[pos++] = prefix[i];
    char pidbuf[20]; int plen = 0;
    unsigned long p = (unsigned long)pid;
    if (!p) { pidbuf[plen++] = '0'; }
    else { char tmp[20]; int tl = 0; while (p) { tmp[tl++] = '0' + (int)(p % 10); p /= 10; }
           for (int i = tl-1; i >= 0; i--) pidbuf[plen++] = tmp[i]; }
    for (int i = 0; i < plen; i++) path[pos++] = pidbuf[i];
    path[pos] = '\0';

    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    int fds[2] = { STDERR_FILENO, fd };

    const char hdr[] = "\n=== ddog_sidecar_direct_entry: fatal signal ===\n";
    for (int i = 0; i < 2; i++) if (fds[i] >= 0) write(fds[i], hdr, sizeof(hdr) - 1);

    // Write signal number and fault/crash addresses using only async-signal-safe ops.
    // backtrace() is NOT called: the and$-16 stack alignment breaks CFI, causing
    // _Unwind_Backtrace to fault.  Instead we extract the IP directly from ucontext.
    static const char hex[] = "0123456789abcdef";
    // Helper: write "label: 0xHEX\n" for an unsigned long
#define WRITE_HEX(label, val) do { \
        const char _lab[] = label ": 0x"; \
        for (int _i = 0; _i < 2; _i++) if (fds[_i] >= 0) write(fds[_i], _lab, sizeof(_lab)-1); \
        char _hbuf[18]; int _hl = 0; unsigned long _v = (unsigned long)(val); \
        if (!_v) { _hbuf[_hl++] = '0'; } \
        else { char _tmp[16]; int _tl = 0; while (_v) { _tmp[_tl++] = hex[_v&0xf]; _v>>=4; } \
               for (int _j=_tl-1;_j>=0;_j--) _hbuf[_hl++]=_tmp[_j]; } \
        _hbuf[_hl++] = '\n'; \
        for (int _i = 0; _i < 2; _i++) if (fds[_i] >= 0) write(fds[_i], _hbuf, _hl); \
    } while(0)

    { int s = sig; char sl[24] = "signal: "; int sll = 8;
      char stmp[10]; int stl = 0;
      if (!s) stmp[stl++] = '0';
      else { while (s > 0) { stmp[stl++] = '0' + s % 10; s /= 10; } }
      for (int i = stl-1; i >= 0; i--) sl[sll++] = stmp[i];
      sl[sll++] = '\n';
      for (int i = 0; i < 2; i++) if (fds[i] >= 0) write(fds[i], sl, sll); }

    if (si) WRITE_HEX("fault_addr", si->si_addr);

#if defined(__linux__) && defined(__x86_64__)
    if (ctx) {
        ucontext_t *uc = (ucontext_t *)ctx;
        WRITE_HEX("rip", uc->uc_mcontext.gregs[REG_RIP]);
        WRITE_HEX("rsp", uc->uc_mcontext.gregs[REG_RSP]);
    }
#elif defined(__linux__) && defined(__aarch64__)
    if (ctx) {
        ucontext_t *uc = (ucontext_t *)ctx;
        WRITE_HEX("pc",  uc->uc_mcontext.pc);
        WRITE_HEX("sp",  uc->uc_mcontext.sp);
    }
#endif
#undef WRITE_HEX

    // Dump /proc/self/maps so RIP can be attributed to a library
#ifdef __linux__
    {
        const char maps_hdr[] = "\n=== /proc/self/maps ===\n";
        for (int i = 0; i < 2; i++) if (fds[i] >= 0) write(fds[i], maps_hdr, sizeof(maps_hdr)-1);
        int mfd = open("/proc/self/maps", O_RDONLY);
        if (mfd >= 0) {
            char mbuf[4096];
            ssize_t n;
            while ((n = read(mfd, mbuf, sizeof(mbuf))) > 0)
                for (int i = 0; i < 2; i++) if (fds[i] >= 0) write(fds[i], mbuf, (size_t)n);
            close(mfd);
        }
    }
#endif

    if (fd >= 0) close(fd);

    struct sigaction sa = { .sa_handler = SIG_DFL };
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
    raise(sig);
}

// Called by ld.so when the library is exec'd directly.
// Linked as the ELF e_entry.
//
// _DD_SIDECAR_DIRECT_EXEC must be set to the name of the symbol to call

// Hidden (not static) so asm can reference it by name.
// @PLT in the call generates R_X86_64_PLT32 which old linkers accept for shared objects.
// 'used' prevents LTO from dropping it (it's only called from the naked asm below).
__attribute__((visibility("hidden"), used, noinline))
void ddog_sidecar_direct_entry_body(void);

// Naked wrapper: ld.so JUMPs (not calls) to e_entry, so rsp alignment is
// unpredictable.  We must align the stack BEFORE the C prologue runs — doing
// it inside the function body is too late because the prologue already anchors
// rbp from the unaligned rsp, causing movaps on rbp-relative locals to fault
// with #GP (reported as SIGSEGV si_addr=0 on Linux).
__attribute__((visibility("default")))
#if defined(__x86_64__)
__attribute__((naked))
void ddog_sidecar_direct_entry(void) {
    __asm__ (
        "and $-16, %rsp\n\t"    /* 16-byte align before C prologue sees rsp */
        "call ddog_sidecar_direct_entry_body@PLT\n\t"  /* @PLT → R_X86_64_PLT32, valid in .so */
        "ud2"                    /* unreachable: body calls _exit */
    );
}
#elif defined(__i386__)
__attribute__((naked))
void ddog_sidecar_direct_entry(void) {
    __asm__ (
        "and $-16, %esp\n\t"
        "call ddog_sidecar_direct_entry_body@PLT\n\t"
        "ud2"
    );
}
#else
void ddog_sidecar_direct_entry(void) {
    ddog_sidecar_direct_entry_body();
}
#endif

__attribute__((visibility("hidden"), used, noinline))
void ddog_sidecar_direct_entry_body(void) {
    // Install crash handler so any fatal signal is captured in a file.
    struct sigaction sa = { .sa_sigaction = crash_handler,
                            .sa_flags = SA_SIGINFO | SA_RESETHAND };
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS,  &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGILL,  &sa, NULL);

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
