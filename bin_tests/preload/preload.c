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
// Flag to indicate we are currently in the collector; we should only
// detect allocations when we are in the collector.
// Must be thread-local: the collector work runs on a single thread; other
// threads in the process should not be considered "collector" and should
// not trip the detector.
static __thread int collector_marked = 0;

// Called by the collector process to enable detection in the collector only
void dd_preload_logger_mark_collector(void) {
    collector_marked = 1;
    if (log_fd >= 0 || !collector_marked) {
        // Already initialized or not a collector
        return;
    }
}

static void write_int(int fd, long value) {
    char buf[32];
    int i = 0;

    if (value == 0) {
        write(fd, "0", 1);
        return;
    }

    if (value < 0) {
        write(fd, "-", 1);
        value = -value;
    }

    while (value > 0 && i < (int)sizeof(buf)) {
        buf[i++] = '0' + (value % 10);
        value /= 10;
    }

    for (int j = i - 1; j >= 0; j--) {
        write(fd, &buf[j], 1);
    }
}

// This function MUST be async signal safe
static void capture_and_report_allocation(const char *func_name) {
    const char *path = "/tmp/preload_detector.log";
    log_fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (log_fd >= 0) {
        pid_t pid = getpid();
        long tid = syscall(SYS_gettid);

        write(log_fd,
              "[FATAL] Dangerous allocation detected in collector!\n",
              52);

        write(log_fd, "  Function: ", 12);
        write(log_fd, func_name, strlen(func_name));

        write(log_fd, "\n  PID: ", 8);
        write_int(log_fd, pid);

        write(log_fd, "\n  TID: ", 8);
        write_int(log_fd, tid);

        close(log_fd);
        log_fd = -1;
        abort();
    }
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
