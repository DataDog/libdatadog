<!-- Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/ -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Examples

Build the Linux `origin` + `rustix` HTTP-only example for a linux-none target
without enabling this crate's `alloc`, `std`, TLS, or mbedtls features. The
example streams a fixed Alpine ISO over HTTP into SHA-256 and checks it against a
hardcoded digest. Name resolution checks `/etc/hosts` first, then uses a
`low_dns` A-record resolver configured from `/etc/resolv.conf`, including
`search`/`domain`, `options ndots:n`, CNAME chasing, and TCP fallback for
truncated UDP responses.

```bash
RUSTFLAGS="-C target-feature=+crt-static -C relocation-model=static" \
  cargo +nightly-2026-02-08 build \
  -p libdd-signal-safe-http-client \
  --example http_only_no_std \
  --no-default-features \
  --target x86_64-unknown-linux-none \
  -Zbuild-std=core,compiler_builtins \
  -Zbuild-std-features=compiler-builtins-mem
```

The static relocation model avoids requiring Origin's experimental PIE
relocation path for a fully static executable.

```bash
docker run --rm --platform linux/amd64 \
  -v "$PWD:/work" \
  -w /work \
  debian:bookworm-slim \
  /work/target/x86_64-unknown-linux-none/debug/examples/http_only_no_std
```

Build and run a scratch image:

```bash
printf 'FROM scratch\nCOPY http_only_no_std /http_only_no_std\nENTRYPOINT ["/http_only_no_std"]\n' \
  | docker build --platform linux/amd64 \
      -t libdd-signal-safe-http-client:http-only-no-std-scratch \
      -f - \
      target/x86_64-unknown-linux-none/debug/examples

docker run --rm --platform linux/amd64 \
  libdd-signal-safe-http-client:http-only-no-std-scratch
```
