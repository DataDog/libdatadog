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

# ------------------------------------------------------------------------------
# Feature sets
#
# Both tools compare one feature set per invocation, so a single run only ever
# sees the API surface that set activates. Checking just one of them masks real
# breakage in both directions: --all-features hides items that exist only while a
# feature is *off* (`#[cfg(not(feature = "x"))]`) and hides a removed feature
# whose items moved under another one, while the default set hides everything
# gated behind an opt-in feature. Each set is therefore diffed independently and
# the highest level wins.
#
# cargo-public-api has no --default-features flag: default features are what it
# activates when given neither --all-features nor --no-default-features.
#
# The `explicit` set is the empty feature list by default, which for some crates
# is not a configuration any consumer can build: libdd-http-client's lib.rs has a
# `compile_error!` demanding a backend. Such a crate declares the minimal set that
# does compile, and it is diffed in place of the empty one:
#
#     [package.metadata.semver-level]
#     explicit-features = ["https", "hyper-backend"]
#
# `explicit-features = false` instead drops the set, for a crate whose only valid
# minimal configuration is already covered by `default`. Declaring either keeps a
# permanent, unactionable build-failure warning out of CI.
# ------------------------------------------------------------------------------
FEATURE_SETS=(all default explicit)

# Emit the cargo-semver-checks flags selecting feature set $1, one per line.
# $2 is the crate's declared explicit-features list (comma separated, may be empty).
semver_checks_feature_args() {
    local set=$1 extra=$2
    case "$set" in
        all)      printf '%s\n' --all-features ;;
        default)  printf '%s\n' --default-features ;;
        explicit)
            printf '%s\n' --only-explicit-features
            if [[ -n "$extra" ]]; then
                printf '%s\n%s\n' --features "$extra"
            fi
            ;;
        *)        echo "Unknown feature set: $set" >&2; return 1 ;;
    esac
}

# Emit the cargo-public-api flags selecting feature set $1, one per line
# (nothing for the default set — see above). $2 as above.
public_api_feature_args() {
    local set=$1 extra=$2
    case "$set" in
        all)      printf '%s\n' --all-features ;;
        default)  ;;
        explicit)
            printf '%s\n' --no-default-features
            if [[ -n "$extra" ]]; then
                printf '%s\n%s\n' --features "$extra"
            fi
            ;;
        *)        echo "Unknown feature set: $set" >&2; return 1 ;;
    esac
}

# Echo, space separated: the crate's feature count, its default-feature count, how
# it configures the `explicit` set ("none" | "features" | "off") and, for
# "features", the declared list. Reads [package.metadata.semver-level] documented
# above. Emits "-1 -1 none" for a crate cargo does not know about.
crate_feature_meta() {
    cargo metadata --format-version=1 --no-deps 2>/dev/null \
        | jq -r --arg crate "$1" '
            [.packages[] | select(.name == $crate)][0] as $p
            | if $p == null then "-1 -1 none"
              else
                (try $p.metadata["semver-level"]["explicit-features"] catch null) as $ef
                | (if $ef == null then "none"
                   elif ($ef | type) == "boolean" then (if $ef then "none" else "off" end)
                   elif ($ef | type) == "array" then "features"
                   else "none" end) as $mode
                | "\($p.features | length) \($p.features.default // [] | length) \($mode) "
                  + (if $mode == "features" then ($ef | join(",")) else "" end)
              end'
}

# Echo the features crate $1 declares for the `explicit` set, or nothing.
crate_explicit_features() {
    local n_all n_default mode feats
    read -r n_all n_default mode feats <<< "$(crate_feature_meta "$1")"
    if [[ "$mode" == "features" ]]; then
        printf '%s' "$feats"
    fi
}

# Echo the feature sets worth running for crate $1, dropping the ones that would
# resolve to an identical feature list:
#   - a crate declaring no features at all collapses to a single set;
#   - an absent or empty `default` makes the default set identical to the
#     explicit one — but only while `explicit` really is the empty set, so a
#     declared explicit-features list keeps both.
#   - `explicit-features = false` drops the explicit set outright, and with it
#     the default set when that is empty too, since both would be the empty list.
#
# `all` is always kept, which is what makes pruning against the *current*
# manifest sound: it is the only set that also activates features which exist
# solely on the baseline side, so a feature deleted by the change under review is
# still compared even when the current manifest no longer mentions it.
crate_feature_sets() {
    local n_all n_default mode feats
    read -r n_all n_default mode feats <<< "$(crate_feature_meta "$1")"

    # Unknown crate: hand back the full list and let the caller's own metadata
    # check produce the error message.
    if [[ -z "$n_all" || "$n_all" == "-1" ]]; then
        printf '%s\n' "${FEATURE_SETS[@]}"
        return
    fi

    local -a sets=(all)
    if (( n_all == 0 )); then
        printf '%s\n' "${sets[@]}"
        return
    fi

    # An empty `default` is the empty feature list, i.e. whatever `explicit`
    # resolves to when nothing is declared — keep just one of the two.
    if (( n_default > 0 )); then
        sets+=(default)
    fi
    if [[ "$mode" != "off" ]]; then
        sets+=(explicit)
    fi

    printf '%s\n' "${sets[@]}"
}

# True when the tool output shows rustdoc failed to build the crate, rather than
# the tool completing and reporting an API difference.
#
# Toggling features can legitimately break compilation for a crate that has not
# declared an explicit-features list (see above). That is a property of the crate,
# not a semver signal, so such a feature set is skipped with a warning instead of
# failing the run. A crate that builds under *no* feature set is still an error.
is_rustdoc_build_failure() {
    grep -qE "could not document|failed to build rustdoc" <<< "$1"
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
    # 0d) Feature sets to compare under (see FEATURE_SETS above).
    # ----------------------------------------------------------------
    local -a feature_sets
    mapfile -t feature_sets < <(crate_feature_sets "$crate")
    local explicit_features
    explicit_features=$(crate_explicit_features "$crate")
    log_verbose "Feature sets for $crate: ${feature_sets[*]}${explicit_features:+ (explicit = $explicit_features)}"

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
        local fs fs_level fs_reason fs_details
        local semver_ran=false
        local -a fs_args

        for fs in "${feature_sets[@]}"; do
            mapfile -t fs_args < <(semver_checks_feature_args "$fs" "$explicit_features")
            SEMVER_OUTPUT=$(cargo semver-checks -p "$crate" --color=never "${fs_args[@]}" --baseline-rev "$baseline" 2>&1)
            SEMVER_EXIT_CODE=$?

            fs_level="none"
            fs_reason=""
            fs_details=""

            if [[ $SEMVER_EXIT_CODE -eq 0 ]]; then
                log_verbose "cargo-semver-checks [features: $fs]: no violations"
                semver_ran=true
            elif [[ $SEMVER_EXIT_CODE -eq 1 ]]; then
                if grep -qE "Summary semver requires new major version" <<< "$SEMVER_OUTPUT"; then
                    fs_level="major"
                    fs_reason="cargo-semver-checks detected breaking changes (features: $fs)"
                    fs_details=$(extract_semver_details "$SEMVER_OUTPUT")
                    semver_ran=true
                    log_verbose "cargo-semver-checks [features: $fs]: major"
                elif grep -qF "package \`$crate\` not found" <<< "$SEMVER_OUTPUT"; then
                    # The crate doesn't exist in the baseline — it's a new crate being
                    # added. (0b) normally catches this first; this is the fallback for
                    # a manifest whose package name the git grep there could not match.
                    semver_level="minor"
                    semver_reason="New crate (not present in baseline)"
                    semver_details=""
                    crate_is_new=true
                    semver_ran=true
                    log_verbose "cargo-semver-checks: new crate, treat as minor"
                    break
                elif grep -qE "Summary semver requires new minor version" <<< "$SEMVER_OUTPUT"; then
                    fs_level="minor"
                    fs_reason="cargo-semver-checks detected minor breaking changes (features: $fs)"
                    fs_details=$(extract_semver_details "$SEMVER_OUTPUT")
                    semver_ran=true
                    log_verbose "cargo-semver-checks [features: $fs]: minor"
                elif is_rustdoc_build_failure "$SEMVER_OUTPUT"; then
                    echo "Warning: $crate does not build under feature set '$fs', skipping it for cargo-semver-checks" >&2
                    log_verbose "$SEMVER_OUTPUT"
                    continue
                else
                    echo "Error running cargo-semver-checks (features: $fs): $SEMVER_OUTPUT" >&2
                    exit $SEMVER_EXIT_CODE
                fi
            else
                echo "Unexpected exit code from cargo-semver-checks (features: $fs): $SEMVER_EXIT_CODE" >&2
                echo "$SEMVER_OUTPUT" >&2
                exit $SEMVER_EXIT_CODE
            fi

            # Keep the highest level seen, along with the feature set that produced it.
            if [[ "$(max_level "$semver_level" "$fs_level")" == "$fs_level" \
                  && "$fs_level" != "$semver_level" ]]; then
                semver_level="$fs_level"
                semver_reason="$fs_reason"
                semver_details="$fs_details"
            fi

            # major is the ceiling; no remaining feature set can raise it.
            if [[ "$semver_level" == "major" ]]; then
                log_verbose "cargo-semver-checks: major reached, skipping remaining feature sets"
                break
            fi
        done

        if ! $semver_ran; then
            echo "Error: $crate does not build under any of the feature sets (${feature_sets[*]}), cargo-semver-checks could not analyze it" >&2
            exit 1
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
        # Sweep the same feature sets as cargo-semver-checks above, so both tools
        # compare the same API surfaces. The sweep is load-bearing for proc-macro
        # crates: cargo-semver-checks is skipped for them, so this is the only
        # comparison, and a removed or renamed macro visible under just one feature
        # set would otherwise pass as a patch.
        local pa_fs pa_level pa_reason pa_details
        local public_api_ran=false
        local -a pa_args

        for pa_fs in "${feature_sets[@]}"; do
            mapfile -t pa_args < <(public_api_feature_args "$pa_fs" "$explicit_features")
            PUBLIC_API_OUTPUT=$(cargo public-api --package "$crate" "${pa_args[@]}" --color=never diff "$baseline..$current" 2>&1)
            EXIT_CODE=$?

            if [[ $EXIT_CODE -ne 0 ]]; then
              if is_rustdoc_build_failure "$PUBLIC_API_OUTPUT"; then
                  echo "Warning: $crate does not build under feature set '$pa_fs', skipping it for cargo-public-api" >&2
                  log_verbose "$PUBLIC_API_OUTPUT"
                  continue
              fi
              echo "Unexpected error from cargo-public-api for $crate (features: $pa_fs, exit code: $EXIT_CODE)" >&2
              echo "$PUBLIC_API_OUTPUT" >&2
              exit $EXIT_CODE
            fi

            public_api_ran=true
            log_verbose "cargo-public-api [features: $pa_fs]:"
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
                    log_verbose "cargo-public-api [features: $pa_fs]: changed items are non-breaking (attribute/qualifier only)"
                fi
            fi

            # Added public items → minor.
            local added=false
            if grep -q "Added items to the public API$" <<< "$PUBLIC_API_OUTPUT" \
               && ! grep -A 2 "^Added items to the public API$" <<< "$PUBLIC_API_OUTPUT" | grep -q "^(none)"; then
                added=true
            fi

            pa_level="none"
            pa_reason=""
            pa_details=""

            if $removed_breaking; then
                pa_level="major"
                pa_reason="cargo-public-api detected removed public API items (features: $pa_fs)"
                pa_details=$(public_api_section "$PUBLIC_API_OUTPUT" \
                    "Removed items from the public API" "Changed items in the public API" \
                    | truncate_details 50)
                log_verbose "cargo-public-api [features: $pa_fs]: major (removed items)"
            elif $changed_breaking; then
                pa_level="major"
                pa_reason="cargo-public-api detected breaking signature changes (features: $pa_fs)"
                pa_details=$(truncate_details 50 <<< "$changed_section")
                log_verbose "cargo-public-api [features: $pa_fs]: major (changed signatures)"
            elif $added; then
                pa_level="minor"
                pa_reason="cargo-public-api detected new public API items (features: $pa_fs)"
                pa_details=$(public_api_section "$PUBLIC_API_OUTPUT" \
                    "Added items to the public API" "" | truncate_details 50)
                log_verbose "cargo-public-api [features: $pa_fs]: minor (added items)"
            fi

            # Keep the highest level seen, along with the feature set that produced it.
            if [[ "$(max_level "$public_api_level" "$pa_level")" == "$pa_level" \
                  && "$pa_level" != "$public_api_level" ]]; then
                public_api_level="$pa_level"
                public_api_reason="$pa_reason"
                public_api_details="$pa_details"
            fi

            # major is the ceiling; no remaining feature set can raise it.
            if [[ "$public_api_level" == "major" ]]; then
                log_verbose "cargo-public-api: major reached, skipping remaining feature sets"
                break
            fi
        done

        if ! $public_api_ran; then
            echo "Error: $crate does not build under any of the feature sets (${feature_sets[*]}), cargo-public-api could not diff it" >&2
            exit 1
        fi
    fi

    # ----------------------------------------------------------------
    # 3) Combine signals: take the higher of cargo-semver-checks and cargo-public-api.
    # ----------------------------------------------------------------
    LEVEL=$(max_level "$semver_level" "$public_api_level")
    if [[ "$LEVEL" == "$public_api_level" && "$public_api_level" != "$semver_level" ]]; then
        REASON="$public_api_reason"
        DETAILS="$public_api_details"
    else
        REASON="$semver_reason"
        DETAILS="$semver_details"
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
