#!/usr/bin/env python3
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
"""Lists the functions of a diagnostic libdd-profiling-ffi build that call
into panic machinery, and the exported entry points that reach them.

    cargo rustc -p libdd-profiling-ffi --profile release-profiling \
        --crate-type cdylib -- --cfg ddprof_ffi_cdylib --cfg ddprof_ffi_diagnose
    scripts/profiling-ffi-panic-callers.py target/release-profiling/libdatadog_profiling_ffi.dylib
"""
import re
import subprocess
import sys

PANIC = re.compile(
    r"rust_begin_unwind|panicking|panic_bounds_check|slice_(start|end)_index|"
    r"slice_index_order|handle_alloc_error|capacity_overflow|unwrap_failed|"
    r"expect_failed|panic_fmt|panic_nounwind|__rust_alloc_error_handler|__rdl_oom"
)


def main(path):
    out = subprocess.run(
        ["objdump", "-d", "--no-show-raw-insn", "-C", path],
        check=True, capture_output=True, text=True,
    ).stdout
    calls = {}
    current = None
    for line in out.splitlines():
        m = re.match(r"^[0-9a-f]+ <(.+)>:$", line)
        if m:
            current = m.group(1)
            calls.setdefault(current, set())
            continue
        m = re.search(r"\b(bl|b|call|jmp)\w*\s+[0-9a-fx]+ <([^>+]+)", line)
        if m and current:
            calls[current].add(m.group(2))
    panicky = {f for f, cs in calls.items() if PANIC.search(f)}
    reaches = set(panicky)
    changed = True
    while changed:
        changed = False
        for f, cs in calls.items():
            if f not in reaches and cs & reaches:
                reaches.add(f)
                changed = True
    direct = sorted(f for f, cs in calls.items() if f not in panicky and cs & panicky)
    print("Functions calling panic machinery directly:")
    for f in direct:
        print("  ", f, "->", ", ".join(sorted(cs for cs in calls[f] if cs in panicky)))
    print("Exported entry points that can reach a panic:")
    for f in sorted(f for f in reaches if "ddog_prof_" in f):
        print("  ", f)


if __name__ == "__main__":
    main(sys.argv[1])
