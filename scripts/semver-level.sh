#!/bin/bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0


VERBOSE=false

# Use GITHUB_OUTPUT from environment or default to /dev/stdout for local testing
if [ -z "$GITHUB_OUTPUT" ]; then
    GITHUB_OUTPUT=/dev/stdout
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        -v|--verbose)
            VERBOSE=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [-v] [-h] CRATE BASE_REF CURRENT_REF"
            exit 0
            ;;
        -*)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
        *)
            # Stop parsing flags, rest are positional
            break
            ;;
    esac
done

CRATE="${1:?ERROR: CRATE is required}"
BASE_REF="${2:-main}"
CURRENT_REF="${3:-HEAD}"

log_verbose() {
    if [ "$VERBOSE" = true ]; then
        echo "$@" >&2
    fi
}

# Echo the higher of two semver levels. Order: major > minor > patch > none.
max_level() {
    local a=$1 b=$2
    local ra rb
    case "$a" in
        major) ra=3 ;;
        minor) ra=2 ;;
        patch) ra=1 ;;
        *)     ra=0 ;;
    esac
    case "$b" in
        major) rb=3 ;;
        minor) rb=2 ;;
        patch) rb=1 ;;
        *)     rb=0 ;;
    esac
    if (( ra >= rb )); then
        echo "$a"
    else
        echo "$b"
    fi
}

# Normalize a cargo-public-api signature line so that only semver-significant
# differences remain. Reads lines on stdin and writes normalized lines out.
# It drops the leading +/- diff marker, removes attribute tokens (`#[...]`) and
# the `const`/`unsafe` qualifiers, and collapses whitespace. Those
# qualifier/attribute deltas are either non-breaking (adding `#[repr(C)]`,
# making a fn `const`) or already covered by cargo-semver-checks' own lints
# (`repr_c_added`, `function_const_removed`, `function_unsafe_added`), so
# stripping them lets us tell a real signature change (e.g. a parameter or
# return type change) from cosmetic churn under "Changed items".
#
# `async` is deliberately NOT stripped. cargo-semver-checks has no async lint at all, so
# unlike const/unsafe there is no second opinion to fall back on
normalize_api_line() {
    sed -E 's/^[+-]//; s/#\[[^]]*\]//g; s/\b(const|unsafe)\b//g; s/[[:space:]]+/ /g; s/^ //; s/ $//'
}

# Cap a block of detail lines at <max>, marking the cut so an elided failure list is not
# mistaken for a complete one. Reads on stdin, writes on stdout.
truncate_details() {
    local max=$1
    local content total
    content=$(cat)

    if [[ -z "$content" ]]; then
        return
    fi

    total=$(wc -l <<< "$content")
    if (( total > max )); then
        head -n "$max" <<< "$content"
        printf '... (%d more lines truncated)\n' "$(( total - max ))"
    else
        printf '%s\n' "$content"
    fi
}

# Pull the failure blocks out of a cargo-semver-checks run, falling back to the tail of
# the output when the run reported a level without emitting any `--- failure` block.
#
# The fallback keeps only the last $tail_lines lines, which drops the *leading* ones —
# the opposite end from what truncate_details cuts. It therefore marks the cut itself:
# truncate_details, seeing only the already-shortened tail, would report no truncation
# and leave the details looking complete.
extract_semver_details() {
    local output=$1
    local tail_lines=50
    local details total
    details=$(grep -A 1000 "^--- failure" <<< "$output")
    if [[ -z "$details" ]]; then
        total=$(wc -l <<< "$output")
        details=$(tail -n "$tail_lines" <<< "$output")
        if (( total > tail_lines )); then
            details=$(printf '... (%d earlier lines omitted)\n%s\n' \
                "$(( total - tail_lines ))" "$details")
        fi
    fi
    truncate_details 100 <<< "$details"
}

# Extract one titled section of a `cargo public-api diff` report, bounded by the next
# section header so the details cannot bleed into unrelated sections.
public_api_section() {
    local output=$1 start=$2 end=$3
    if [[ -n "$end" ]]; then
        sed -n "/^${start}$/,/^${end}$/p" <<< "$output"
    else
        sed -n "/^${start}$/,\$p" <<< "$output"
    fi
}

# Echo the unique target kinds ("lib", "bin", "proc-macro", ...) cargo reports for
# crate $1 in the workspace rooted at $2 (default: the current checkout).
crate_target_kinds() {
    local crate=$1 root=${2:-.}
    cargo metadata --format-version=1 --no-deps --manifest-path "$root/Cargo.toml" 2>/dev/null \
        | jq -r --arg crate "$crate" \
            '[.packages[] | select(.name == $crate) | .targets[].kind[]] | unique | join(" ")'
}

# Echo the target kinds at a specific revision. crate ($1) at revision ($2).
crate_target_kinds_at_rev() {
    local crate=$1 rev=$2
    local tree kinds
    tree=$(mktemp -d) || return 1
    if ! git archive "$rev" | tar -x -C "$tree"; then
        echo "Error: could not extract $rev to read target kinds for $crate" >&2
        rm -rf "$tree"
        return 1
    fi
    kinds=$(crate_target_kinds "$crate" "$tree")
    rm -rf "$tree"
    echo "$kinds"
}

# Echo one tab-separated row per manifest fact about crate $1 at revision $2, read once
# per revision for both manifest passes below:
#
#   dep      <name> <alias> <kind> <target> <req>   one per dependency a consumer resolves
#   feature  <name> default|optional                one per declared feature
#
# Read via `cargo metadata` rather than the crate's own Cargo.toml, which would see
# neither the version behind a `{ workspace = true }` inheritance marker nor the implicit
# feature an `optional = true` dependency creates.
#
# <alias> is `.rename`, defaulting to the package name. It is in the row because a crate
# may alias two versions of one package (`foo1`/`foo2` both `package = "foo"`), for which
# cargo metadata reports an identical name, kind and target; keyed without it, the second
# alias matches the first's baseline requirement and an unchanged pair reads as a raise.
crate_manifest_facts_at_rev() {
    local crate=$1 rev=$2
    local tree meta cargo_status
    tree=$(mktemp -d) || return 1
    if ! git archive "$rev" | tar -x -C "$tree"; then
        echo "Error: could not extract $rev to read the manifest for $crate" >&2
        rm -rf "$tree"
        return 1
    fi
    # Captured before jq: without pipefail a failing cargo would surface as jq's clean
    # exit over empty input, and a broken manifest would read as "declares nothing".
    meta=$(cargo metadata --format-version=1 --no-deps --manifest-path "$tree/Cargo.toml" 2>/dev/null)
    cargo_status=$?
    rm -rf "$tree"
    if [[ $cargo_status -ne 0 || -z "$meta" ]]; then
        echo "Error: could not read the manifest for $crate at $rev" >&2
        return 1
    fi
    jq -r --arg crate "$crate" '
        .packages[]
        | select(.name == $crate)
        | . as $p
        | (
            ( $p.dependencies[]
              | select((.kind // "normal") != "dev")
              | ["dep", .name, (.rename // .name), (.kind // "normal"), (.target // "any"), .req] ),
            ( ($p.features // {}) as $f
              | ($f.default // []) as $d
              | ($f | keys[]) as $n
              | select($n != "default")
              | ["feature", $n, (if ($d | index($n)) then "default" else "optional" end)] )
          )
        | @tsv' <<< "$meta"
}

# version_gt A B — true when the dotted numeric tuple A is strictly greater than B.
# Four components: req_min appends an exclusivity flag to the X.Y.Z triple.
# Compared in the shell rather than with `sort -V`, a GNU extension.
version_gt() {
    local -a a=() b=()
    local i x y
    IFS='.' read -r -a a <<< "$1"
    IFS='.' read -r -a b <<< "$2"
    for i in 0 1 2 3; do
        x=${a[i]:-0}
        y=${b[i]:-0}
        if (( 10#$x > 10#$y )); then
            return 0
        elif (( 10#$x < 10#$y )); then
            return 1
        fi
    done
    return 1
}

# Echo the lowest version requirement $1 admits, as `X.Y.Z.E` with E=1 when the bound
# EXCLUDES that version. `>1.2.3` and `>=1.2.3` are different floors, so without E the
# two compare equal and narrowing one into the other passes as a patch. E is an ordering
# device only; the report quotes requirements verbatim.
#
# Echo nothing when there is no lower bound (`*`, `<2`) or it cannot be parsed: callers
# treat that as "no opinion", so an exotic requirement can never invent a bump. A
# pre-release bound is compared as its release version (`1.0.0-rc.1` -> `1.0.0`), which
# can only understate a raise.
req_min() {
    local req=$1
    local -a parts=() v=()
    local part bound best="" exclusive
    read -r -a parts <<< "${req//,/ }"
    for part in "${parts[@]}"; do
        exclusive=0
        case "$part" in
            ''|'*'|'<'*|'!='*) continue ;;
            '>='*)  bound=${part#>=} ;;
            '>'*)   bound=${part#>}; exclusive=1 ;;
            '^'*)   bound=${part#^} ;;
            '~'*)   bound=${part#\~} ;;
            '='*)   bound=${part#=} ;;
            [0-9]*) bound=$part ;;
            *)      continue ;;
        esac
        bound=${bound%%[-+]*}   # drop pre-release and build metadata
        bound=${bound//\*/0}    # `1.*` admits 1.0.0
        [[ "$bound" =~ ^[0-9]+(\.[0-9]+)*$ ]] || continue
        IFS='.' read -r -a v <<< "$bound"
        bound="${v[0]:-0}.${v[1]:-0}.${v[2]:-0}.${exclusive}"
        if [[ -z "$best" ]] || version_gt "$bound" "$best"; then
            best=$bound
        fi
    done
    if [[ -n "$best" ]]; then
        printf '%s\n' "$best"
    fi
}

compute_semver_results() {
    local crate=$1
    local baseline=$2
    local current=$3

    # If current is not provided set it to the tip of the branch
    if [ -z "$current" ]; then
        current="HEAD"
    fi

    # Fetch base commit
    git fetch origin "$baseline" --quiet
    local fetch_exit_code=$?
    if [[ $fetch_exit_code -ne 0 ]]; then
        echo "Failed to fetch baseline ref: $baseline" >&2
        return "$fetch_exit_code"
    fi

    # Ensure baseline has origin/ prefix if it doesn't already (skip for tags: refs/tags/...)
    if [[ ! "$baseline" =~ ^origin/ ]] && [[ "$baseline" != *"refs/tags"* ]]; then
        baseline="origin/$baseline"
    fi

    log_verbose "========================================"
    log_verbose "Checking semver for: $crate"
    log_verbose "Using baseline ref: $baseline"
    log_verbose "========================================"

    # ----------------------------------------------------------------
    # 0) Select proper tool
    #
    # cargo-semver-checks only lints crates with a *library* target. A crate
    # declaring `proc-macro = true` reports its target kind as `proc-macro`, not
    # `lib`, so cargo-semver-checks selects nothing and exits 1 with "no crates
    # with library targets selected". 
    # cargo-public-api *does* support proc-macro crates — it reports the exported
    # macros (e.g. `pub proc macro libdd_ipc_macros::#[service]`) — so a removed
    # or renamed macro is still caught as a major change.
    # ----------------------------------------------------------------
    local target_kinds has_lib=false has_proc_macro=false
    target_kinds=$(crate_target_kinds "$crate")

    if [[ -z "$target_kinds" ]]; then
        echo "Error: $crate has no targets in cargo metadata (unknown crate?)" >&2
        exit 1
    fi

    if [[ " $target_kinds " == *" lib "* ]]; then
        has_lib=true
    fi
    if [[ " $target_kinds " == *" proc-macro "* ]]; then
        has_proc_macro=true
    fi
    log_verbose "Target kinds for $crate: $target_kinds"

    # ----------------------------------------------------------------
    # 0b) Is the crate absent from the baseline, i.e. added by this PR?
    #
    # Decide this independently of the tool selection above. Inferring it from
    # cargo-semver-checks' "package not found" output only works for crates that
    # reach cargo-semver-checks at all: a crate without a library target skips it,
    # so a newly added proc-macro crate would fall through to cargo-public-api,
    # which would then try to build a baseline package that does not exist and
    # fail — instead of reporting the intended `minor`.
    #
    # Match the package name declared in any manifest at the baseline rev rather
    # than a fixed path, so relocating a crate's directory is not mistaken for
    # adding a new one.
    # ----------------------------------------------------------------
    local crate_is_new=false
    if ! git grep -q -E "^name = \"${crate}\"\$" "$baseline" -- '*Cargo.toml' 2>/dev/null; then
        crate_is_new=true
        log_verbose "$crate is absent from $baseline: new crate"
    fi

    # ----------------------------------------------------------------
    # 0c) Check changes in the crate type.
    #
    # If crate changes its type the following block will detect it in order to
    # adjust the semver level changes since current tools will miss that case.
    # ----------------------------------------------------------------
    local baseline_kinds="" baseline_has_lib=false baseline_has_proc_macro=false
    if ! $crate_is_new; then
        if ! baseline_kinds=$(crate_target_kinds_at_rev "$crate" "$baseline"); then
            exit 1
        fi
        if [[ " $baseline_kinds " == *" lib "* ]]; then
            baseline_has_lib=true
        fi
        if [[ " $baseline_kinds " == *" proc-macro "* ]]; then
            baseline_has_proc_macro=true
        fi
        log_verbose "Target kinds for $crate at $baseline: ${baseline_kinds:-none}"
    fi

    # A crate carries a comparable public API only through its library or
    # proc-macro target.
    local api_before=false api_now=false
    if $baseline_has_lib || $baseline_has_proc_macro; then
        api_before=true
    fi
    if $has_lib || $has_proc_macro; then
        api_now=true
    fi

    local target_change_level="none" target_change_reason=""
    if $api_before && ! $api_now; then
        target_change_level="major"
        target_change_reason="Library/proc-macro target removed (baseline: $baseline_kinds, now: $target_kinds)"
    elif ! $api_before && $api_now && ! $crate_is_new; then
        target_change_level="minor"
        target_change_reason="Library/proc-macro target added (baseline: ${baseline_kinds:-none}, now: $target_kinds)"
    fi

    # ----------------------------------------------------------------
    # 1) cargo-semver-checks (type-signature lints) — library targets only.
    # ----------------------------------------------------------------
    local semver_level="none"
    local semver_reason=""
    local semver_details=""

    if $crate_is_new; then
        # Nothing to compare against; adding a crate is a minor change.
        semver_level="minor"
        semver_reason="New crate (not present in baseline)"
        log_verbose "Skipping cargo-semver-checks: new crate, treat as minor"
    elif [[ "$target_change_level" != "none" ]]; then
        # Decided in (0c): the crate gained or lost its whole API surface, which
        # neither tool can diff.
        semver_level="$target_change_level"
        semver_reason="$target_change_reason"
        log_verbose "Skipping cargo-semver-checks: $target_change_reason"
    elif ! $has_lib || ! $baseline_has_lib; then
        log_verbose "Skipping cargo-semver-checks: $crate has no library target on both revs (baseline: ${baseline_kinds:-none}, now: $target_kinds)"
    else
        SEMVER_OUTPUT=$(cargo semver-checks -p "$crate" --color=never --all-features --baseline-rev "$baseline" 2>&1)
        SEMVER_EXIT_CODE=$?

        if [[ $SEMVER_EXIT_CODE -eq 0 ]]; then
            log_verbose "cargo-semver-checks: no violations"
            semver_level="none"
        elif [[ $SEMVER_EXIT_CODE -eq 1 ]]; then
            if grep -qE "Summary semver requires new major version" <<< "$SEMVER_OUTPUT"; then
                semver_level="major"
                semver_reason="cargo-semver-checks detected breaking changes"
                semver_details=$(extract_semver_details "$SEMVER_OUTPUT")
                log_verbose "cargo-semver-checks: major"
            elif grep -qF "package \`$crate\` not found" <<< "$SEMVER_OUTPUT"; then
                # The crate doesn't exist in the baseline — it's a new crate being added
                semver_level="minor"
                semver_reason="New crate (not present in baseline)"
                crate_is_new=true
                log_verbose "cargo-semver-checks: new crate, treat as minor"
            elif grep -qE "Summary semver requires new minor version" <<< "$SEMVER_OUTPUT"; then
                semver_level="minor"
                semver_reason="cargo-semver-checks detected minor breaking changes"
                semver_details=$(extract_semver_details "$SEMVER_OUTPUT")
                log_verbose "cargo-semver-checks: minor"
            else
                echo "Error running cargo-semver-checks: $SEMVER_OUTPUT" >&2
                exit $SEMVER_EXIT_CODE
            fi
        else
            echo "Unexpected exit code from cargo-semver-checks: $SEMVER_EXIT_CODE" >&2
            echo "$SEMVER_OUTPUT" >&2
            exit $SEMVER_EXIT_CODE
        fi
    fi

    # ----------------------------------------------------------------
    # 2) cargo-public-api diff
    #
    # cargo-semver-checks has known false-negatives at signature level — most
    # notably, parameter type changes on non-generic functions are not detected
    # (the function_parameter_type_changed lint is not implemented). Such a change
    # keeps the item's path, so cargo-public-api reports it under "Changed items"
    # as a "-old / +new" signature pair (not as a Removed + Added pair). We
    # therefore run cargo-public-api unconditionally and, for changed items,
    # normalize and compare the old vs new signatures (see the Changed handling
    # below) before combining the result with semver-checks via max_level. Skip
    # only when there is no baseline (new crate), when semver-checks already
    # flagged major (cannot go higher), or when either rev lacks a library and a
    # proc-macro target, leaving cargo-public-api nothing to diff on that side.
    #
    # Requires cargo-public-api >= 0.52.0: earlier versions include function
    # parameter names in signatures, so a non-breaking parameter *rename* also
    # surfaces under "Changed items" (-old / +new differing only by the name) and
    # would be falsely promoted to major. From 0.52.0 parameter names are omitted
    # by default, so a rename produces no diff and only signature-meaningful
    # changes (e.g. parameter or return type changes) surface there.
    # ----------------------------------------------------------------
    local public_api_level="none"
    local public_api_reason=""
    local public_api_details=""

    if $crate_is_new; then
        log_verbose "Skipping cargo-public-api: new crate (no baseline)"
    elif [[ "$semver_level" == "major" ]]; then
        log_verbose "Skipping cargo-public-api: cargo-semver-checks already at major"
    elif ! $api_now || ! $api_before; then
        # Nothing to diff: at least one rev has no library or proc-macro target.
        # When that is a change rather than the status quo, (0c) already scored it.
        log_verbose "Skipping cargo-public-api: $crate has no library or proc-macro target on both revs (baseline: ${baseline_kinds:-none}, now: $target_kinds)"
    else
        # --all-features matches the cargo-semver-checks invocation above, so both
        # tools compare the same API surface. It is load-bearing for proc-macro
        # crates: cargo-semver-checks is skipped for them, so this is the only
        # comparison, and under the default feature set a removed or renamed
        # feature-gated macro would be invisible and pass as a patch.
        PUBLIC_API_OUTPUT=$(cargo public-api --package "$crate" --all-features --color=never diff "$baseline..$current" 2>&1)
        EXIT_CODE=$?

        if [[ $EXIT_CODE -ne 0 ]]; then
          echo "Unexpected error from cargo-public-api for $crate (exit code: $EXIT_CODE)" >&2
          echo "$PUBLIC_API_OUTPUT" >&2
          exit $EXIT_CODE
        fi

        log_verbose "$PUBLIC_API_OUTPUT"

        # Removed public items → major.
        local removed_breaking=false
        if grep -q "Removed items from the public API$" <<< "$PUBLIC_API_OUTPUT" \
           && ! grep -A 2 "^Removed items from the public API$" <<< "$PUBLIC_API_OUTPUT" | grep -q "^(none)$"; then
            removed_breaking=true
        fi

        # Changed public items → breaking only if a semver-significant delta
        # survives normalization. This is the case cargo-semver-checks misses:
        # a parameter/return type change on a non-generic fn renders here as a
        # "-old / +new" signature pair rather than as Removed+Added. We compare
        # the normalized old vs new signatures; if they still differ after
        # stripping non-breaking churn (#[repr(C)] additions, const/async/unsafe
        # qualifiers — see normalize_api_line), the change is breaking → major.
        local changed_breaking=false
        local changed_section
        changed_section=$(sed -n '/^Changed items in the public API$/,/^Added items to the public API$/p' <<< "$PUBLIC_API_OUTPUT")
        if [[ -n "$changed_section" ]] \
           && ! grep -A 2 "^Changed items in the public API$" <<< "$PUBLIC_API_OUTPUT" | grep -q "^(none)$"; then
            local changed_old changed_new
            changed_old=$(grep '^-' <<< "$changed_section" | normalize_api_line | sort)
            changed_new=$(grep '^+' <<< "$changed_section" | normalize_api_line | sort)
            if [[ "$changed_old" != "$changed_new" ]]; then
                changed_breaking=true
            else
                log_verbose "cargo-public-api: changed items are non-breaking (attribute/qualifier only)"
            fi
        fi

        # Added public items → minor.
        local added=false
        if grep -q "Added items to the public API$" <<< "$PUBLIC_API_OUTPUT" \
           && ! grep -A 2 "^Added items to the public API$" <<< "$PUBLIC_API_OUTPUT" | grep -q "^(none)"; then
            added=true
        fi

        if $removed_breaking; then
            public_api_level="major"
            public_api_reason="cargo-public-api detected removed public API items"
            public_api_details=$(public_api_section "$PUBLIC_API_OUTPUT" \
                "Removed items from the public API" "Changed items in the public API" \
                | truncate_details 50)
            log_verbose "cargo-public-api: major (removed items)"
        elif $changed_breaking; then
            public_api_level="major"
            public_api_reason="cargo-public-api detected breaking signature changes"
            public_api_details=$(truncate_details 50 <<< "$changed_section")
            log_verbose "cargo-public-api: major (changed signatures)"
        elif $added; then
            public_api_level="minor"
            public_api_reason="cargo-public-api detected new public API items"
            public_api_details=$(public_api_section "$PUBLIC_API_OUTPUT" \
                "Added items to the public API" "" | truncate_details 50)
            log_verbose "cargo-public-api: minor (added items)"
        fi
    fi

    # ----------------------------------------------------------------
    # 2b) Dependency requirement floors
    #
    # Neither tool above reads a manifest. Raising the lowest version a requirement
    # admits (`http = "1"` -> `"1.1"`) leaves every rustdoc signature byte-identical, so
    # both report nothing and the level comes out patch — yet a consumer pinned to the
    # old version can no longer resolve this crate, conventionally a minor.
    #
    # Deliberately NOT scored: dev-dependencies (not part of what a consumer resolves);
    # a widened requirement; a dependency added or removed outright. Nor a raised MAJOR
    # floor, capped at minor here on purpose — whether that forces the dependent to
    # major is release-version-major-bumps.sh's call, made with the dependency graph
    # this script cannot see, and max_level() lets it win from there.
    # ----------------------------------------------------------------
    local dep_level="none"
    local dep_reason=""
    local dep_details=""
    local feature_level="none"
    local feature_reason=""
    local feature_details=""

    # The passes have different ceilings, so they stop at different points: (2b) can
    # only report minor, so minor ends it; (2c) can report major, so it runs on until
    # major. They share one extraction per revision, gated by (2c)'s wider ceiling.
    #
    # Stopping (2c) at minor too would save that extraction, since (1)'s
    # `feature_missing` / `feature_not_enabled_by_default` cover its major findings —
    # but only while those lints exist, and the failure mode is silent. Second opinion
    # kept on purpose.
    local level_so_far
    level_so_far=$(max_level "$semver_level" "$public_api_level")

    local base_facts="" now_facts="" manifest_compared=false
    if $crate_is_new; then
        log_verbose "Skipping manifest diff: new crate (no baseline)"
    elif [[ "$level_so_far" == "major" ]]; then
        log_verbose "Skipping manifest diff: already at major"
    else
        if ! base_facts=$(crate_manifest_facts_at_rev "$crate" "$baseline"); then
            exit 1
        fi
        if ! now_facts=$(crate_manifest_facts_at_rev "$crate" "$current"); then
            exit 1
        fi
        manifest_compared=true
    fi

    if $manifest_compared && [[ "$level_so_far" == "minor" ]]; then
        log_verbose "Skipping dependency requirement diff: already at minor, which is this pass's ceiling"
    elif $manifest_compared; then
        local base_reqs now_reqs raised=""
        base_reqs=$(awk -F'\t' '$1 == "dep" { print substr($0, index($0, "\t") + 1) }' <<< "$base_facts")
        now_reqs=$(awk -F'\t' '$1 == "dep" { print substr($0, index($0, "\t") + 1) }' <<< "$now_facts")

        local dep_name dep_alias dep_kind dep_target dep_req dep_label old_req old_min new_min
        while IFS=$'\t' read -r dep_name dep_alias dep_kind dep_target dep_req; do
            [[ -z "$dep_name" ]] && continue
            # Name, alias, kind and target together: see crate_manifest_facts_at_rev for
            # why the alias belongs in the key.
            old_req=$(awk -F'\t' -v n="$dep_name" -v a="$dep_alias" -v k="$dep_kind" -v t="$dep_target" \
                '$1 == n && $2 == a && $3 == k && $4 == t { print $5; exit }' <<< "$base_reqs")
            # Absent from the baseline: a dependency newly added, or one whose alias
            # changed. Either way not a raised floor.
            [[ -z "$old_req" ]] && continue
            [[ "$old_req" == "$dep_req" ]] && continue

            dep_label="$dep_name"
            [[ "$dep_alias" != "$dep_name" ]] && dep_label="$dep_name as $dep_alias"

            old_min=$(req_min "$old_req")
            new_min=$(req_min "$dep_req")
            if [[ -z "$old_min" || -z "$new_min" ]]; then
                log_verbose "Not judging $dep_label: unparsed requirement ($old_req -> $dep_req)"
                continue
            fi
            if version_gt "$new_min" "$old_min"; then
                raised+="$dep_label ($dep_kind): $old_req -> $dep_req"$'\n'
                log_verbose "$dep_label floor raised: $old_req -> $dep_req"
            fi
        done <<< "$now_reqs"

        if [[ -n "$raised" ]]; then
            dep_level="minor"
            dep_reason="Dependency requirement floor raised"
            dep_details=$(truncate_details 50 <<< "${raised%$'\n'}")
        fi
    fi

    # ----------------------------------------------------------------
    # 2c) Cargo feature surface
    #
    # Features are public API — downstream writes `features = ["x"]` — so adding one is
    # a minor and removing one, or dropping it from the default set, breaks consumers
    # that named it.
    #
    # An ADDED feature is the gap: cargo-semver-checks has no lint for it (0.47/0.48
    # offer feature_missing, feature_not_enabled_by_default and the two
    # *_enables_feature lints, nothing for an addition) and it adds no rustdoc item
    # unless it gates one, so it used to come out patch.
    #
    # The removal cases are a backstop. For a library crate feature_missing and
    # feature_not_enabled_by_default get there first (verified to fail the run, not
    # merely warn) and this block is skipped once a pass said major; it earns its keep
    # on a crate with no library target, where (1) is skipped entirely.
    #
    # Not scored: a change to what a feature *enables* — the two *_enables_feature
    # lints' job, and judging it needs the feature graph rather than a name set.
    # ----------------------------------------------------------------
    if $manifest_compared; then
        local base_features now_features
        local feat_name feat_default removed="" added="" undefaulted=""
        base_features=$(awk -F'\t' '$1 == "feature" { print $2 "\t" $3 }' <<< "$base_facts")
        now_features=$(awk -F'\t' '$1 == "feature" { print $2 "\t" $3 }' <<< "$now_facts")

        while IFS=$'\t' read -r feat_name feat_default; do
            [[ -z "$feat_name" ]] && continue
            if ! awk -F'\t' -v n="$feat_name" '$1 == n { found = 1 } END { exit !found }' <<< "$now_features"; then
                removed+="$feat_name"$'\n'
                continue
            fi
            # Still declared, but no longer reached by `default`.
            if [[ "$feat_default" == "default" ]] \
               && ! awk -F'\t' -v n="$feat_name" '$1 == n && $2 == "default" { found = 1 } END { exit !found }' <<< "$now_features"; then
                undefaulted+="$feat_name"$'\n'
            fi
        done <<< "$base_features"

        while IFS=$'\t' read -r feat_name feat_default; do
            [[ -z "$feat_name" ]] && continue
            if ! awk -F'\t' -v n="$feat_name" '$1 == n { found = 1 } END { exit !found }' <<< "$base_features"; then
                added+="$feat_name"$'\n'
            fi
        done <<< "$now_features"

        if [[ -n "$removed" || -n "$undefaulted" ]]; then
            feature_level="major"
            feature_reason="Cargo feature removed or no longer enabled by default"
            feature_details=$(truncate_details 50 <<< "$(
                [[ -n "$removed" ]] && printf 'removed: %s\n' "${removed//$'\n'/ }"
                [[ -n "$undefaulted" ]] && printf 'no longer default: %s\n' "${undefaulted//$'\n'/ }"
            )")
            log_verbose "features removed: ${removed//$'\n'/ } undefaulted: ${undefaulted//$'\n'/ }"
        elif [[ -n "$added" ]]; then
            feature_level="minor"
            feature_reason="Cargo feature added"
            feature_details=$(truncate_details 50 <<< "added: ${added//$'\n'/ }")
            log_verbose "features added: ${added//$'\n'/ }"
        fi
    fi

    # ----------------------------------------------------------------
    # 3) Combine: the highest level any pass reported. The reason comes from the pass
    # that decided it, and on a tie from the one that describes the change most
    # concretely — a removed item says more than "a dependency floor moved".
    # ----------------------------------------------------------------
    LEVEL=$(max_level "$semver_level" "$public_api_level")
    LEVEL=$(max_level "$LEVEL" "$feature_level")
    LEVEL=$(max_level "$LEVEL" "$dep_level")
    if [[ "$semver_level" == "$LEVEL" ]]; then
        REASON="$semver_reason"
        DETAILS="$semver_details"
    elif [[ "$public_api_level" == "$LEVEL" ]]; then
        REASON="$public_api_reason"
        DETAILS="$public_api_details"
    elif [[ "$feature_level" == "$LEVEL" ]]; then
        REASON="$feature_reason"
        DETAILS="$feature_details"
    else
        REASON="$dep_reason"
        DETAILS="$dep_details"
    fi

    if [[ "$LEVEL" == "none" ]]; then
        LEVEL="patch"
        REASON="No public API changes detected"
    fi

    jq -n \
        --arg name "$crate" \
        --arg level "$LEVEL" \
        --arg reason "$REASON" \
        --arg details "$DETAILS" \
        '{"name": $name, "level": $level, "reason": $reason, "details": $details}'
}

# Run the computation and capture JSON output.
#
# compute_semver_results runs in a command substitution, so its `exit` calls only
# terminate that subshell. Without propagating the status here the script would
# return 0 with empty stdout, and the caller would report a confusing "unknown
# level ()" instead of the underlying tool error.
RESULT_JSON=$(compute_semver_results "$CRATE" "$BASE_REF" "$CURRENT_REF")
RESULT_EXIT_CODE=$?

if [[ $RESULT_EXIT_CODE -ne 0 ]] || [[ -z "$RESULT_JSON" ]]; then
    echo "Error: failed to compute semver level for $CRATE (exit code: $RESULT_EXIT_CODE)" >&2
    exit "$(( RESULT_EXIT_CODE == 0 ? 1 : RESULT_EXIT_CODE ))"
fi

# Output JSON to stdout (captured by workflow)
echo "$RESULT_JSON"

# Extract values from JSON for backwards compatibility / local testing
NAME=$(echo "$RESULT_JSON" | jq -r '.name')
LEVEL=$(echo "$RESULT_JSON" | jq -r '.level')

# For local testing, also output individual values
if [[ "$GITHUB_OUTPUT" == "/dev/stdout" ]]; then
  echo "---" >&2
  echo "crate=$NAME" >&2
  echo "semver_level=$LEVEL" >&2
fi
