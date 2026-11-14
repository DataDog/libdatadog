#!/usr/bin/env python3
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
"""Linker wrapper for stable-benchmarks.py: links as `cc` would, and records the command so
the executable can be linked again later with another code layout, without recompiling.

rustc deletes some of the linker's inputs once the link is done (the crate's own object
files, `symbols.o`, response files under $TMPDIR), so those are copied next to the record
and the recorded command points at the copies. Everything else it reads (rlibs under the
target directory, the toolchain's libraries) outlives the build.

Environment: STABLE_LINK_RECORD_DIR (where records go; unset: just link) and
STABLE_REAL_LINKER (the linker to run, default `cc`)."""
import json
import os
import shutil
import sys


def read_response_file(path):
    """Arguments of a GNU-style response file as rustc writes one: one per line, with `\\`
    escaping the next character."""
    args = []
    with open(path) as f:
        for line in f.read().split('\n'):
            arg, chars = [], iter(line)
            for c in chars:
                arg.append(next(chars, '') if c == '\\' else c)
            if line:
                args.append(''.join(arg))
    return args


def main():
    linker = os.environ.get('STABLE_REAL_LINKER', 'cc')
    args = sys.argv[1:]
    record_dir = os.environ.get('STABLE_LINK_RECORD_DIR')
    if record_dir:
        record(record_dir, args, linker)
    os.execvp(linker, [linker] + args)


def record(record_dir, args, linker):
    expanded = []
    for arg in args:
        expanded += read_response_file(arg[1:]) if arg.startswith('@') else [arg]
    output = next((expanded[i + 1] for i, a in enumerate(expanded[:-1]) if a == '-o'), None)
    name = os.path.basename(output or '')
    # Build scripts are linked by the hundred and never benchmarked.
    if not name or name.startswith('build_script_'):
        return
    tmp = os.path.realpath(os.environ.get('TMPDIR', '/tmp')) + os.sep
    kit = os.path.join(record_dir, name)
    shutil.rmtree(kit, ignore_errors=True)
    os.makedirs(kit)
    kept = []
    for i, arg in enumerate(expanded):
        path = os.path.realpath(arg) if os.path.exists(arg) else ''
        ephemeral = path and arg != output and (path.startswith(tmp) or arg.endswith('.rcgu.o'))
        if ephemeral:
            copy = os.path.join(kit, f'{i:04d}-{os.path.basename(arg)}')
            if os.path.isdir(path):
                shutil.copytree(path, copy)
            else:
                shutil.copy2(path, copy)
            arg = copy
        kept.append(arg)
    with open(os.path.join(record_dir, name + '.json'), 'w') as f:
        json.dump({'cwd': os.getcwd(), 'linker': linker, 'output': output, 'args': kept}, f)


if __name__ == '__main__':
    main()
