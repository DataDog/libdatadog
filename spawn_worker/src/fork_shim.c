// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <sys/types.h>
#include <unistd.h>

#if defined(__linux__)
#include <sys/syscall.h>
#endif

#if !defined(__APPLE__)
extern pid_t _Fork(void) __attribute__((weak));
// On supported Darwin versions (macOS 12+), vfork() is fork() with a private
// "LibSystem handlers only" flag. This skips API pthread_atfork handlers while
// retaining LibSystem's pthread, malloc, libc, dyld, dispatch, and XPC repair
// hooks. That is the closest Darwin implementation to POSIX _Fork().
#endif

pid_t ddog_spawn_worker_fork(int *error) {
  pid_t result;
#if !defined(__APPLE__)
  if (_Fork != NULL) {
    result = _Fork();
  } else
#endif
  {
#if defined(__linux__)
#ifdef SYS_fork
    result = (pid_t)syscall(SYS_fork);
#else
    result = (pid_t)syscall(SYS_clone, SIGCHLD, NULL, NULL, NULL, 0);
#endif
#elif defined(__APPLE__)
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    result = vfork();
#pragma clang diagnostic pop
#else
#error "Unsupported platform"
#endif
  }

  // musl's _Fork calls __post_Fork after this syscall to repair its libc thread
  // bookkeeping. The fallback cannot call that private function, so its child
  // must use raw syscalls or wrappers known not to depend on unrepaired libc
  // state until exec or _exit; spawn_worker enforces that stricter contract.

  *error = result == -1 ? errno : 0;
  return result;
}
