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

static void *(*real_malloc)(size_t) = NULL;
static void (*real_free)(void *) = NULL;
static void *(*real_calloc)(size_t, size_t) = NULL;
static void *(*real_realloc)(void *, size_t) = NULL;
static int log_fd = -1;
static pthread_once_t init_once = PTHREAD_ONCE_INIT;

static int is_enabled(void) {
    const char *v = getenv("MALLOC_LOG_ENABLED");
    return v && v[0] == '1';
}

static void init_logger(void) {
    const char *path = getenv("MALLOC_LOG_PATH");
    if (path == NULL || path[0] == '\0') {
        path = "/tmp/malloc_logger.log";
    }

    log_fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    real_malloc = dlsym(RTLD_NEXT, "malloc");
    real_free = dlsym(RTLD_NEXT, "free");
    real_calloc = dlsym(RTLD_NEXT, "calloc");
    real_realloc = dlsym(RTLD_NEXT, "realloc");
}

static void log_line(const char *tag, size_t size, void *ptr) {
    if (log_fd < 0) {
        return;
    }

    if (!is_enabled()) {
        return;
    }

    char buf[200];
    pid_t pid = getpid();
    long tid = syscall(SYS_gettid);
    int len;

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
    pthread_once(&init_once, init_logger);

    if (real_malloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    void *ptr = real_malloc(size);
    log_line("malloc", size, ptr);
    return ptr;
}

void free(void *ptr) {
    pthread_once(&init_once, init_logger);

    if (real_free == NULL) {
        return;
    }

    log_line("free", 0, ptr);
    real_free(ptr);
}

void *calloc(size_t nmemb, size_t size) {
    pthread_once(&init_once, init_logger);

    if (real_calloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    void *ptr = real_calloc(nmemb, size);
    log_line("calloc", nmemb * size, ptr);
    return ptr;
}

void *realloc(void *ptr, size_t size) {
    pthread_once(&init_once, init_logger);

    if (real_realloc == NULL) {
        errno = ENOMEM;
        return NULL;
    }

    void *new_ptr = real_realloc(ptr, size);
    log_line("realloc", size, new_ptr);
    return new_ptr;
}
