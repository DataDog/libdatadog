# FFE sidecar forwarder diagrams

Architecture diagram for libdatadog PR #2026 — the FFE (Feature Flag
Evaluation) sidecar forwarders.

| Diagram | Purpose |
| --- | --- |
| `system-pr2026.mmd` / `.png` | End-to-end view from tracer payload → FFI → sidecar dispatcher → flusher → backend, with PR scope highlighted. |

The PR introduces two `SidecarAction` arms (`FfeExposures`, `FfeMetrics`)
and two flusher modules (`ffe_exposures_flusher` for the Agent EVP
proxy, `ffe_metrics_flusher` for an OTLP HTTP intake). FFE actions are
lifted out of the `applications.entry(queue_id)` gate in
`enqueue_actions` because they are session-scoped (the trace endpoint
or the caller-supplied OTLP URL is all they need) — see commit
`875ec8f0e` and the dispatcher in `datadog-sidecar/src/service/sidecar_server.rs`.

## Regenerating the PNG

```sh
cd /path/to/libdatadog
npx --yes @mermaid-js/mermaid-cli@latest \
  -i docs/ffe-sidecar/system-pr2026.mmd \
  -o docs/ffe-sidecar/system-pr2026.png \
  -w 2400 -H 2400 --scale 3 -b white
```

`-w 2400 -H 2400 --scale 3 -b white` yields a crisp PNG that reads
well on the PR page and survives zooming. The first `npx` invocation
downloads a headless Chromium (~150 MB, ~60 s); subsequent runs are
fast.

The diagram uses `flowchart TD` (top-to-bottom) and the YAML `title:`
is quoted so the `#PR-number` is not parsed as a comment by Mermaid's
frontmatter parser.
