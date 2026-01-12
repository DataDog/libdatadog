// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

static void *(*real_malloc)(size_t) = NULL;
static void (*real_free)(void *) = NULL;
static void *(*real_calloc)(size_t, size_t) = NULL;
static void *(*real_realloc)(void *, size_t) = NULL;
static int log_fd = -1;
static pthread_once_t init_once = PTHREAD_ONCE_INIT;
// Flag to indicate we are currently in the collector; We should only
// log when we are in the collector.
static int collector_marked = 0;

static void init_function_ptrs(void) {
    if (real_malloc == NULL) {
        real_malloc = dlsym(RTLD_NEXT, "malloc");
        real_free = dlsym(RTLD_NEXT, "free");
        real_calloc = dlsym(RTLD_NEXT, "calloc");
        real_realloc = dlsym(RTLD_NEXT, "realloc");
    }
}

static void init_logger(void) {
    if (log_fd >= 0 || !collector_marked) {
        // Already initialized or not a collector
        return;
    }

    const char *path = "/tmp/preload_logger.log";
    log_fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (log_fd >= 0) {
        char buf[256];
        pid_t pid = getpid();
        int len = snprintf(buf, sizeof(buf), "[DEBUG] Collector logger initialized pid=%d, fd=%d, path=%s\n",
                          pid, log_fd, path);
        write(log_fd, buf, len);
    }
}

// Called by the collector process to scope logging to the collector only
void dd_preload_logger_mark_collector(void) {
    collector_marked = 1;

    init_logger();

    if (log_fd >= 0) {
        char buf[256];
        pid_t pid = getpid();
        int len = snprintf(buf, sizeof(buf), "[DEBUG] Marked as collector, pid=%d, fd=%d\n", pid, log_fd);
        write(log_fd, buf, len);
    }
}

static void log_line(const char *tag, size_t size, void *ptr) {

    if (log_fd < 0 || !collector_marked) {
        return;
    }

    char buf[200];
    pid_t pid = getpid();
    long tid = syscall(SYS_gettid);
    int len = 0;

    if (strcmp(tag, "malloc") == 0) {
        len = snprintf(buf, sizeof(buf), "pid=%d tid=%ld malloc size=%zu ptr=%p\n", pid, tid, size, ptr);
    } else if (strcmp(tag, "calloc") == 0) {
        len = snprintf(buf, sizeof(buf), "pid=%d tid=%ld calloc size=%zu ptr=%p\n", pid, tid, size, ptr);
    } else if (strcmp(tag, "realloc") == 0) {
        len = snprintf(buf, sizeof(buf), "pid=%d tid=%ld realloc size=%zu ptr=%p\n", pid, tid, size, ptr);
    } else if (strcmp(tag, "free") == 0) {
        len = snprintf(buf, sizeof(buf), "pid=%d tid=%ld free ptr=%p\n", pid, tid, ptr);
    }

    if (len > 0) {
        (void)write(log_fd, buf, (size_t)len);
    }
}

void *malloc(size_t size) {
    pthread_once(&init_once, init_function_ptrs);

    if (real_malloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    void *ptr = real_malloc(size);
    if (collector_marked) {
        log_line("malloc", size, ptr);
    }
    return ptr;
}

void free(void *ptr) {
    pthread_once(&init_once, init_function_ptrs);

    if (real_free == NULL) {
        return;
    }

    if (collector_marked) {
        log_line("free", 0, ptr);
    }
    real_free(ptr);
}

void *calloc(size_t nmemb, size_t size) {
    pthread_once(&init_once, init_function_ptrs);

    if (real_calloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    void *ptr = real_calloc(nmemb, size);
    if (collector_marked) {
        log_line("calloc", nmemb * size, ptr);
    }
    return ptr;
}

void *realloc(void *ptr, size_t size) {
    pthread_once(&init_once, init_function_ptrs);

    if (real_realloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    void *new_ptr = real_realloc(ptr, size);
    if (collector_marked) {
        log_line("realloc", size, new_ptr);
    }
    return new_ptr;
}
