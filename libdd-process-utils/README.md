# libdd-process-utils

Process management, thread identification, and timeout utilities for Datadog
libraries.

## Modules

- `unix_utils`: Unix-only process management.
  - `PreparedExecve` converts the binary path, arguments, and environment into
    `CString`s in `new()`; the struct keeps them alive, so `exec()` is a bare
    `execve` syscall on the stored pointers and nothing allocates between fork
    and exec. The crash tracker prepares one before forking a receiver.
  - `alt_fork` starts the child through the raw `clone()` syscall, skipping
    the atfork handlers that glibc `fork()` would run, and passes
    `CLONE_PTRACE` when the parent is being traced so the tracer keeps
    following the child; on macOS it is a plain `fork()`.
  - `reap_child_non_blocking` spins on `waitpid(WNOHANG)` until the
    `TimeoutManager` deadline; `wait_for_pollhup` polls a file descriptor for
    `POLLHUP` with the same deadline. `terminate` ends the current process
    immediately via `_exit(EXIT_FAILURE)`, without atexit handlers or abort.
  - `restricted_file` provides constrained opens for paths supplied by worker
    processes: it rejects `procfs`/`sysfs` targets and resolves magic links
    with `openat2(RESOLVE_NO_MAGICLINKS)` where available (an
    `O_PATH`/`O_NOFOLLOW` component walk otherwise), so a worker that dropped
    to another UID cannot reach the master's memory layout or bypass directory
    permissions.
- `threading`: numeric identifiers for the current OS thread (Linux, macOS,
  Windows).
- `timeout`: `TimeoutManager`, the deadline tracker used to bound collector
  shutdown sequences. `remaining()` is floored at 160ms (a few scheduler
  slices), so a shutdown poll never gets a zero window.

`unix_utils` compiles to nothing on non-Unix targets; the crate itself still
builds there for `threading` and `timeout`.
