// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#ifdef __GLIBC__
#include <execinfo.h>
#define DD_HAVE_EXECINFO 1
#else
#define DD_HAVE_EXECINFO 0
#endif

static void *(*real_malloc)(size_t) = NULL;
static void (*real_free)(void *) = NULL;
static void *(*real_calloc)(size_t, size_t) = NULL;
static void *(*real_realloc)(void *, size_t) = NULL;
static pthread_once_t init_once = PTHREAD_ONCE_INIT;

// We should load all the real symbols on library load
static void init_function_ptrs(void) {
    if (real_malloc == NULL) {
        real_malloc = dlsym(RTLD_NEXT, "malloc");
        real_free = dlsym(RTLD_NEXT, "free");
        real_calloc = dlsym(RTLD_NEXT, "calloc");
        real_realloc = dlsym(RTLD_NEXT, "realloc");
    }
}

__attribute__((constructor)) static void preload_ctor(void) {
    pthread_once(&init_once, init_function_ptrs);
}

static int log_fd = -1;
// Flag to indicate we are currently in the collector; We should only
// detect allocations when we are in the collector.
static int collector_marked = 0;
// Flag to track if we've already detected and reported an allocation
// This guards against reentrancy of the detection logic when we capture
// stack trace, since backtrace can use malloc internally
static int allocation_detected = 0;

// Called by the collector process to enable detection in the collector only
void dd_preload_logger_mark_collector(void) {
    collector_marked = 1;
    if (log_fd >= 0 || !collector_marked) {
        // Already initialized or not a collector
        return;
    }
}

static void capture_and_report_allocation(const char *func_name) {
    // Only report once using atomic compare-and-swap
    if (__sync_bool_compare_and_swap(&allocation_detected, 0, 1)) {
        const char *path = "/tmp/preload_detector.log";
        log_fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
        if (log_fd >= 0) {
            char buf[4096];
            pid_t pid = getpid();
            long tid = syscall(SYS_gettid);

            // Log the detection
            int len = snprintf(buf, sizeof(buf),
                "[FATAL] Dangerous allocation detected in collector!\n"
                "  Function: %s\n"
                "  PID: %d\n"
                "  TID: %ld\n"
                "  Stacktrace:\n",
                func_name, pid, tid);
            write(log_fd, buf, len);

            // Capture and log stacktrace (glibc only; musl lacks execinfo)
#if DD_HAVE_EXECINFO
            void *array[100];
            int size = backtrace(array, 100);
            char **strings = backtrace_symbols(array, size);

            if (strings != NULL) {
                for (int i = 0; i < size; i++) {
                    len = snprintf(buf, sizeof(buf), "    #%d %s\n", i, strings[i]);
                    write(log_fd, buf, len);
                }
                // backtrace_symbols uses malloc internally, so we have a small leak
                // but this is acceptable since this only happens once and we guard
                // against it anyways
            }
#else
            len = snprintf(buf, sizeof(buf),
                "    [backtrace unavailable: execinfo.h not present on this platform (likely musl)]\n");
            write(log_fd, buf, len);
#endif

            fsync(log_fd);
            close(log_fd);
            log_fd = -1;
        }
    }

    // Don't abort. let the collector continue so it can finish writing the crash report
    // The test will check for the log file and fail if allocations were detected
}

void *malloc(size_t size) {
    if (real_malloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    if (collector_marked) {
        capture_and_report_allocation("malloc");
    }

    void *ptr = real_malloc(size);
    return ptr;
}

void free(void *ptr) {
    if (real_free == NULL) {
        return;
    }

    // free is generally safe; we'll allow free operations without failing
    real_free(ptr);
}

void *calloc(size_t nmemb, size_t size) {
    if (real_calloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    if (collector_marked) {
        capture_and_report_allocation("calloc");
    }

    void *ptr = real_calloc(nmemb, size);
    return ptr;
}

void *realloc(void *ptr, size_t size) {
    if (real_realloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    if (collector_marked) {
        capture_and_report_allocation("realloc");
    }

    void *new_ptr = real_realloc(ptr, size);
    return new_ptr;
}
