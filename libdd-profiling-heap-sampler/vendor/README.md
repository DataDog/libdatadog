<!--
 Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
 SPDX-License-Identifier: Apache-2.0
-->
# Vendored headers

## `usdt.h`

The libbpf project's single-header USDT (User Statically-Defined
Tracepoint) library. Provides `USDT(group, name, args...)` and friends
that emit the standard v3 ELF-note format consumed by bpftrace,
systemtap, and any BPF-based tracer.

We use it in preference to systemtap's `<sys/sdt.h>` because libbpf/usdt
is genuinely standalone: a single file with no `sdt-config.h` companion
and no other compile-time dependencies on the host system.

- **Upstream:** <https://github.com/libbpf/usdt>
- **Path in upstream:** `usdt.h` (repo root)
- **License:** BSD-2-Clause. The SPDX header is retained verbatim at the
  top of the file. Copyright (c) 2024 Meta Platforms, Inc. and affiliates.
- **Vendored from:** upstream `main` as of 2026-06-29.

To refresh, fetch the latest copy and overwrite the file in place:

```sh
curl -fsSL https://raw.githubusercontent.com/libbpf/usdt/main/usdt.h \
  > libdd-profiling-heap-sampler/vendor/usdt.h
```

Verify the BSD-2-Clause SPDX identifier is still on line 1 after
refreshing.
