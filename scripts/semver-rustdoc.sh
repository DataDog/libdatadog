#!/bin/bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Rustdoc-JSON generation and the content-keyed baseline cache for the
# semver pipeline (semver-level.sh) and the CI warming job. Sourced by
# semver-level.sh; also runnable directly (see usage at the bottom).
#
# SEMVER_RUSTDOC_CACHE_DIR enables the local cache dir, SEMVER_RUSTDOC_S3_URI
# the S3 one; lookups go local dir → S3 → generate. Uploads run only with
# SEMVER_RUSTDOC_CACHE_WRITE=true, set solely on main-branch runs, matching
# the roles' trust: every ref reads, only main writes, so a PR cannot plant
# a rustdoc JSON that trusted builds later consume.

SEMVER_RUSTDOC_CACHE_DIR=${SEMVER_RUSTDOC_CACHE_DIR:-}
SEMVER_RUSTDOC_S3_URI=${SEMVER_RUSTDOC_S3_URI:-}
SEMVER_RUSTDOC_CACHE_WRITE=${SEMVER_RUSTDOC_CACHE_WRITE:-}

log_verbose() { return 0; }   # redefined by callers that want progress logs

# Shared rustdoc flags. -Zunstable-options --output-format=json needs
# RUSTC_BOOTSTRAP=1 on stable, which is also how cargo-semver-checks emits
# JSON from a stable toolchain.
base_rustdoc_flags() {
    printf -- '-Z unstable-options --output-format=json --cap-lints=allow'
}

# flavor_flags <semver|public> — the item flags each tool's own rustdoc
# invocation passes. The public flavor must not include private or
# doc(hidden) items: cargo-public-api has no visibility filter of its own.
flavor_flags() {
    case "$1" in
        semver) printf -- '--document-private-items --document-hidden-items' ;;
        public) printf -- '' ;;
        *) echo "unknown rustdoc flavor: $1 (expected semver or public)" >&2; return 1 ;;
    esac
}

_sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | cut -d' ' -f1
    else
        shasum -a 256 | cut -d' ' -f1
    fi
}

# The lib target name of $1 per cargo metadata run in $2 (default: CWD).
# cargo rustdoc writes target/doc/<lib name, - replaced by _>.json; a custom
# [lib] name is already carried by the target's .name field.
semver_rustdoc_lib_name() {
    local crate=$1 root=${2:-.}
    cargo metadata --format-version=1 --no-deps --manifest-path "$root/Cargo.toml" 2>/dev/null \
        | jq -r --arg crate "$crate" '
            [.packages[]
             | select(.name == $crate)
             | .targets[]
             | select(any(.kind[]; . == "lib" or . == "proc-macro"))
             | .name][0]
            // empty'
}

# semver_rustdoc_generate <crate> <flavor> <workdir> <outfile>
# Generate the rustdoc JSON of crate $1 from the workspace rooted at $3
# (a repo checkout or an extracted baseline tree) into $4. Deps build into
# $3/target, so per-crate invocations against the same tree share dep
# artifacts.
semver_rustdoc_generate() {
    local crate=$1 flavor=$2 workdir=$3 outfile=$4
    local lib
    lib=$(semver_rustdoc_lib_name "$crate" "$workdir")
    if [[ -z "$lib" ]]; then
        echo "Error: $crate has no library or proc-macro target to rustdoc" >&2
        return 1
    fi

    local extra
    extra=$(flavor_flags "$flavor") || return 1

    ( cd "$workdir" \
        && RUSTC_BOOTSTRAP=1 \
           RUSTDOCFLAGS="${RUSTDOCFLAGS:-} $(base_rustdoc_flags) $extra" \
           RUSTFLAGS="${RUSTFLAGS:-} --cap-lints=allow" \
           cargo rustdoc -p "$crate" --all-features --quiet ) || return 1

    local json="${workdir}/target/doc/${lib//-/_}.json"
    if [[ ! -s "$json" ]]; then
        echo "Error: cargo rustdoc produced no JSON for $crate at $json" >&2
        return 1
    fi
    cp "$json" "$outfile"
}

# effective_hash <crate> <rev> <tree-dir>
# Print the cache key for $1's rustdoc JSON as of git revision $2, where $3 is
# an extraction (or checkout) of that revision to read manifests from. The
# key covers everything that can move the JSON: the crate's source subtree,
# the subtrees of its workspace path-deps (a changed proc-macro or build
# script changes this crate's items with its own sources untouched),
# Cargo.lock (external dep macros can generate items in this crate), and
# the rustdoc version.
effective_hash() {
    local crate=$1 rev=$2 tree_dir=$3
    local repo
    repo=$(git rev-parse --show-toplevel) || return 1

    local meta
    meta=$(cargo metadata --format-version=1 --no-deps --manifest-path "$tree_dir/Cargo.toml" 2>/dev/null) || {
        echo "Error: could not read workspace metadata at $tree_dir" >&2
        return 1
    }

    # Workspace members and their raw manifest deps: only names that are
    # themselves members can be followed. cargo metadata reports these
    # without resolving the graph, so this works on bare extractions. Dev
    # deps are excluded: rustdoc builds the lib target only.
    local member_lines
    member_lines=$(jq -r --arg root "$tree_dir" '
        .packages[]
        | select(.source == null)
        | .name + "\t" + (.manifest_path | sub($root + "/"; "") | sub("/Cargo.toml"; ""))
        + "\t" + ([.dependencies[]
                   | select((.kind // "normal") != "dev")
                   | (.rename // .name)] | join(","))' <<< "$meta") || return 1

    # Transitive closure over member names, starting at $crate.
    local closure=() queue=("$crate") seen="" name line deps dep
    while (( ${#queue[@]} )); do
        name=${queue[0]}
        queue=("${queue[@]:1}")
        grep -qxF "$name" <<< "$seen" && continue
        seen+="$name"$'\n'
        line=$(awk -F'\t' -v n="$name" '$1 == n' <<< "$member_lines")
        if [[ -z "$line" ]]; then
            echo "Error: $name is not a workspace member at $rev" >&2
            return 1
        fi
        closure+=("$line")
        for dep in $(cut -f3 <<< "$line" | tr ',' ' '); do
            if awk -F'\t' -v n="$dep" '$1 == n { found=1 } END { exit !found }' <<< "$member_lines"; then
                queue+=("$dep")
            fi
        done
    done

    # Subtree sha per closure member: any change under the dir moves it.
    local parts=() crate_name dir subtree
    while IFS=$'\t' read -r crate_name dir _; do
        subtree=$(git -C "$repo" rev-parse "$rev:$dir" 2>/dev/null) || {
            echo "Error: no subtree $dir at $rev" >&2
            return 1
        }
        parts+=("$crate_name $subtree")
    done < <(printf '%s\n' "${closure[@]}")

    # Fall back to the whole tree sha on repos without a lockfile.
    local locksha
    locksha=$(git -C "$repo" rev-parse "$rev:Cargo.lock" 2>/dev/null) \
        || locksha=$(git -C "$repo" rev-parse "$rev^{tree}")

    {
        printf 'v1|%s|%s|%s|%s|all-features\n' "$crate" "$locksha" \
            "$(rustdoc --version 2>/dev/null)" \
            "$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
        printf '%s\n' "${parts[@]}" | sort
    } | _sha256
}

# s3_fetch <crate> <hash> <flavor> <outfile> — 0 on hit. Any error (no role
# assumed, no aws CLI, network down) counts as a miss: the caller
# regenerates, so a broken cache only costs speed.
s3_fetch() {
    local crate=$1 hash=$2 flavor=$3 outfile=$4
    [[ -n "$SEMVER_RUSTDOC_S3_URI" ]] || return 1
    aws s3 cp --quiet "${SEMVER_RUSTDOC_S3_URI}/${crate}/${hash}.${flavor}.json" \
        "$outfile.part" 2>/dev/null \
        && [[ -s "$outfile.part" ]] \
        && mv "$outfile.part" "$outfile"
}

# s3_store <crate> <hash> <flavor> <file> — upload; a no-op without
# SEMVER_RUSTDOC_CACHE_WRITE (runs holding only the read role).
s3_store() {
    local crate=$1 hash=$2 flavor=$3 file=$4
    [[ "$SEMVER_RUSTDOC_CACHE_WRITE" == "true" && -n "$SEMVER_RUSTDOC_S3_URI" ]] || return 0
    aws s3 cp --quiet "$file" "${SEMVER_RUSTDOC_S3_URI}/${crate}/${hash}.${flavor}.json"
}

# cache_entry_present <crate> <hash> <flavor> — 0 when the entry exists in
# the local dir or in S3. An empty `aws s3 ls` listing exits 0, so the output
# itself is the test.
cache_entry_present() {
    local crate=$1 hash=$2 flavor=$3
    if [[ -n "$SEMVER_RUSTDOC_CACHE_DIR" \
        && -s "${SEMVER_RUSTDOC_CACHE_DIR}/${crate}/${hash}.${flavor}.json" ]]; then
        return 0
    fi
    [[ -n "$SEMVER_RUSTDOC_S3_URI" ]] || return 1
    [[ -n "$(aws s3 ls "${SEMVER_RUSTDOC_S3_URI}/${crate}/${hash}.${flavor}.json" 2>/dev/null)" ]]
}

# semver_rustdoc_baseline <crate> <rev> <tree-dir> <flavor> <outfile>
# Cache-aware wrapper around semver_rustdoc_generate: on a hit the baseline
# JSON comes from the local dir or S3 instead of being rebuilt.
semver_rustdoc_baseline() {
    local crate=$1 rev=$2 tree_dir=$3 flavor=$4 outfile=$5
    local hash entry

    if [[ -z "$SEMVER_RUSTDOC_CACHE_DIR" && -z "$SEMVER_RUSTDOC_S3_URI" ]]; then
        semver_rustdoc_generate "$crate" "$flavor" "$tree_dir" "$outfile"
        return $?
    fi

    hash=$(effective_hash "$crate" "$rev" "$tree_dir") || return 1
    entry="${SEMVER_RUSTDOC_CACHE_DIR}/${crate}/${hash}.${flavor}.json"
    if [[ -s "$entry" ]]; then
        log_verbose "baseline rustdoc cache hit (local) for $crate ($flavor)"
        cp "$entry" "$outfile"
        return 0
    fi
    if s3_fetch "$crate" "$hash" "$flavor" "$outfile"; then
        log_verbose "baseline rustdoc cache hit (S3) for $crate ($flavor)"
        return 0
    fi
    semver_rustdoc_generate "$crate" "$flavor" "$tree_dir" "$outfile" || return 1
    if [[ -n "$SEMVER_RUSTDOC_CACHE_DIR" ]]; then
        mkdir -p "${SEMVER_RUSTDOC_CACHE_DIR}/${crate}"
        cp "$outfile" "$entry.new" && mv "$entry.new" "$entry"
    fi
    s3_store "$crate" "$hash" "$flavor" "$outfile"
    return 0
}

# published_lib_crates [dir] — names of the workspace's published crates that
# have a library or proc-macro target (the only ones with a rustdoc JSON),
# one per line.
published_lib_crates() {
    local root=${1:-.}
    cargo metadata --format-version=1 --no-deps --manifest-path "$root/Cargo.toml" 2>/dev/null \
        | jq -r '.packages[]
            | select(.source == null)
            | select(.publish == null or (.publish | type == "array" and length > 0))
            | select(any(.targets[]; any(.kind[]; . == "lib" or . == "proc-macro")))
            | .name'
}

# semver_rustdoc_warm [dir] — generate the cache entries missing for every
# published lib crate at HEAD. Prints the number of newly generated entries.
semver_rustdoc_warm() {
    local tree_dir=${1:-.} crate flavor generated=0 total=0 hash tmp
    if [[ -z "$SEMVER_RUSTDOC_CACHE_DIR" && -z "$SEMVER_RUSTDOC_S3_URI" ]]; then
        echo "no cache configured (SEMVER_RUSTDOC_CACHE_DIR / SEMVER_RUSTDOC_S3_URI); warming is pointless" >&2
        return 1
    fi
    for crate in $(published_lib_crates "$tree_dir"); do
        total=$((total + 1))
        hash=$(effective_hash "$crate" HEAD "$tree_dir") || return 1
        for flavor in semver public; do
            cache_entry_present "$crate" "$hash" "$flavor" && continue
            tmp=$(mktemp)
            if ! semver_rustdoc_generate "$crate" "$flavor" "$tree_dir" "$tmp"; then
                rm -f "$tmp"
                return 1
            fi
            if [[ -n "$SEMVER_RUSTDOC_CACHE_DIR" ]]; then
                mkdir -p "${SEMVER_RUSTDOC_CACHE_DIR}/${crate}"
                cp "$tmp" "${SEMVER_RUSTDOC_CACHE_DIR}/${crate}/${hash}.${flavor}.json"
            fi
            if ! s3_store "$crate" "$hash" "$flavor" "$tmp"; then
                rm -f "$tmp"
                return 1
            fi
            rm -f "$tmp"
            generated=$((generated + 1))
        done
    done
    echo "warmed $total crates, generated $generated new entries" >&2
    printf '%d\n' "$generated"
}

# semver_rustdoc_prune <rev> — delete cache entries whose hash is not live
# at $1, plus the prefixes of crates that no longer exist. S3 when
# SEMVER_RUSTDOC_S3_URI is set, the local dir otherwise. Anything pruned can
# be regenerated on a miss.
semver_rustdoc_prune() {
    local rev=$1 tree_dir crate live_hash line name
    if [[ -z "$SEMVER_RUSTDOC_CACHE_DIR" && -z "$SEMVER_RUSTDOC_S3_URI" ]]; then
        return 0
    fi

    # Manifests of the rev, via a throwaway extraction (the checkout itself
    # may be at a different rev).
    tree_dir=$(mktemp -d) || return 1
    if ! git archive "$rev" | tar -x -C "$tree_dir"; then
        rm -rf "$tree_dir"
        return 1
    fi

    if [[ -z "$SEMVER_RUSTDOC_S3_URI" ]]; then
        local crate_dir entry
        for crate_dir in "$SEMVER_RUSTDOC_CACHE_DIR"/*/; do
            [[ -d "$crate_dir" ]] || continue
            crate=$(basename "$crate_dir")
            live_hash=$(effective_hash "$crate" "$rev" "$tree_dir" || true)
            for entry in "$crate_dir"/*.json; do
                [[ -e "$entry" ]] || continue
                [[ "$(basename "$entry")" == "${live_hash}."*".json" ]] || rm -f "$entry"
            done
        done
        rm -rf "$tree_dir"
        return 0
    fi

    local live_crates=""
    for crate in $(published_lib_crates "$tree_dir"); do
        live_crates+="$crate"$'\n'
        live_hash=$(effective_hash "$crate" "$rev" "$tree_dir") || { rm -rf "$tree_dir"; return 1; }
        # `aws s3 ls` lines: "<date> <time> <size> <key-suffix>"; the key
        # carries no spaces, so the last field is it regardless of the size.
        while IFS= read -r line; do
            name=$(awk '{print $NF}' <<< "$line")
            [[ -n "$name" ]] || continue
            [[ "$name" == "${live_hash}."*".json" ]] || aws s3 rm "${SEMVER_RUSTDOC_S3_URI}/${crate}/${name}"
        done < <(aws s3 ls "${SEMVER_RUSTDOC_S3_URI}/${crate}/" 2>/dev/null)
    done

    # "PRE <crate>/" lines for prefixes no longer in the workspace.
    while IFS= read -r line; do
        crate=$(awk '{print $NF}' <<< "$line")
        crate=${crate%/}
        [[ -n "$crate" ]] || continue
        grep -qxF "$crate" <<< "$live_crates" || aws s3 rm --recursive "${SEMVER_RUSTDOC_S3_URI}/${crate}/"
    done < <(aws s3 ls "${SEMVER_RUSTDOC_S3_URI}/" 2>/dev/null | grep '^PRE')
    rm -rf "$tree_dir"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    if [[ "${1:-}" == "-v" || "${1:-}" == "--verbose" ]]; then
        log_verbose() { echo "$@" >&2; }
        shift
    fi

    usage() {
        echo "usage: $0 [-v] {generate|warm|prune} ..." >&2
        echo "  generate <crate> <flavor> <dir> <outfile>   one crate's rustdoc JSON (flavor: semver|public)" >&2
        echo "  warm [dir]    fill the baseline cache for HEAD (needs SEMVER_RUSTDOC_CACHE_DIR)" >&2
        echo "  prune <rev>   drop cache entries not live at <rev>" >&2
    }

    cmd=${1:-}
    case "$cmd" in
        generate)
            [[ $# -eq 5 ]] || { usage; exit 1; }
            semver_rustdoc_generate "$2" "$3" "$4" "$5"
            ;;
        warm)
            semver_rustdoc_warm "${2:-.}"
            ;;
        prune)
            [[ $# -eq 2 ]] || { usage; exit 1; }
            semver_rustdoc_prune "$2"
            ;;
        *)
            usage
            exit 1
            ;;
    esac
fi
