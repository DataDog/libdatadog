#!/bin/bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0


VERBOSE=false
LIST_MODE=""

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
        --list-affected)
            LIST_MODE="affected"
            shift
            ;;
        --list-raised-floors)
            LIST_MODE="raised-floors"
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [-v] [-h] CRATE BASE_REF CURRENT_REF"
            echo "       $0 [-v] --list-affected BASE_REF CURRENT_REF"
            echo "       $0 [-v] --list-raised-floors BASE_REF CURRENT_REF"
            echo ""
            echo "With no list flag: print the semver level CRATE needs, as JSON."
            echo ""
            echo "--list-affected prints, one per line, every workspace member whose"
            echo "dependency requirements or feature surface moved between the two"
            echo "revisions -- the crates worth running the first form on, including those"
            echo "a changed-file-path search cannot find because the edit was to the root"
            echo "manifest's [workspace.dependencies]. Any difference counts, since any of"
            echo "them is a reason to check the crate."
            echo ""
            echo "--list-raised-floors prints only the dependency requirement floors that"
            echo "ROSE, tab-separated as <crate> <dep> <kind> <old req> <new req>. That is"
            echo "the subset the first form scores a minor for, so a caller can say which"
            echo "crates a bump is owed to and why. A widened, lowered or unparseable"
            echo "requirement, a dependency added or removed, and a feature change are all"
            echo "absent: they are not raised floors and do not earn a minor on their own."
            echo ""
            echo "Both list modes take BASE_REF named as a ref to mean \"since this branch"
            echo "forked\", and compare from the merge base. A full commit SHA is taken to mean"
            echo "that commit exactly, which differs only when it is not an ancestor of"
            echo "CURRENT_REF -- a release tag off a squash-merged branch, say."
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

if [[ -n "$LIST_MODE" ]]; then
    CRATE=""
    BASE_REF="${1:-main}"
    CURRENT_REF="${2:-HEAD}"
else
    CRATE="${1:?ERROR: CRATE is required}"
    BASE_REF="${2:-main}"
    CURRENT_REF="${3:-HEAD}"
fi

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
# per revision for both manifest passes below. Every workspace member is read when $1 is
# empty, which is how list_affected_crates gets the whole workspace for one extraction.
#
#   <crate> dep      <name> <alias> <kind> <target> <req> <feats> <defaults>  per dependency
#   <crate> feature  <name> default|optional                                  per feature
#
# <feats> and <defaults> are the features the crate turns on in that dependency, and
# whether it takes the dependency's defaults. They are here to be SELECTED on, not
# scored: enabling a dependency feature can change this crate's own API — an item
# reached through a glob re-export appears — but the manifest alone cannot say whether
# it did, and the passes that read rustdoc can. Without them a root
# [workspace.dependencies] entry that only gains a feature leaves every inheriting
# member's rows identical, list_affected_crates names nobody, and with just the root
# manifest touched the workflow skips the jobs that would have looked. Pass 2b keys the
# floor comparison on the columns before these, so a feature move never reads as a
# requirement change.
#
# `default` on a feature row means `default` REACHES the feature, not that it lists it:
# the closure over the feature graph, which is what a consumer writing
# `default-features = true` actually gets. Keyed on direct membership instead, moving
# `default = ["foo"]` to `default = ["bundle"]` with `bundle = ["foo"]` changes nothing
# a consumer can observe yet reads as `foo` losing its default, i.e. a major; and the
# inverse, emptying a `bundle` that `default` still lists, is a real break that reads as
# no change at all. cargo-semver-checks resolves the closure — its
# `feature_not_enabled_by_default` passes the first and fails the second, verified
# against 0.47/0.48 — so keying on direct membership would also put this pass at odds
# with the lint backing it up.
#
# The crate leads every row so that rows from different members stay apart; the passes
# below, which ask for one crate, cut it back off.
#
# Read via `cargo metadata` rather than the crate's own Cargo.toml, which would see
# neither the version behind a `{ workspace = true }` inheritance marker nor the implicit
# feature an `optional = true` dependency creates.
#
# <alias> is `.rename`, defaulting to the package name. It is in the row because a crate
# may alias two versions of one package (`foo1`/`foo2` both `package = "foo"`), for which
# cargo metadata reports an identical name, kind and target; keyed without it, the second
# alias matches the first's baseline requirement and an unchanged pair reads as a raise.
manifest_facts_at_rev() {
    local crate=$1 rev=$2
    local tree meta cargo_status label=${crate:-the workspace}
    tree=$(mktemp -d) || return 1
    if ! git archive "$rev" | tar -x -C "$tree"; then
        echo "Error: could not extract $rev to read the manifest for $label" >&2
        rm -rf "$tree"
        return 1
    fi
    # Captured before jq: without pipefail a failing cargo would surface as jq's clean
    # exit over empty input, and a broken manifest would read as "declares nothing".
    meta=$(cargo metadata --format-version=1 --no-deps --manifest-path "$tree/Cargo.toml" 2>/dev/null)
    cargo_status=$?
    rm -rf "$tree"
    if [[ $cargo_status -ne 0 || -z "$meta" ]]; then
        echo "Error: could not read the manifest for $label at $rev" >&2
        return 1
    fi
    jq -r --arg crate "$crate" '
        # The local features one entry of a feature list enables. `dep:x` activates an
        # optional dependency and deliberately creates no implicit feature; `x?/f`
        # enables `f` of `x` only where `x` is already on, so neither reaches a local
        # feature. Plain `x/f` does also enable the implicit feature `x`, but only when
        # `x` is an optional dependency — which is exactly when `x` is a key of the
        # feature map, a non-optional dependency having none.
        def local_edges($f):
            [ .[]
              | if startswith("dep:") then empty
                elif contains("/") then (split("/")[0] | if endswith("?") then empty else . end)
                else . end ]
            | map(. as $k | select($f | has($k)))
            | unique;
        # Every feature `default` reaches, directly or through another feature. One
        # round per declared feature reaches the fixed point: a round that changes
        # anything adds at least one of the finitely many features.
        def default_closure($f):
            reduce range(0; ($f | length) + 1) as $_
                (($f["default"] // []) | local_edges($f);
                 (. + ([ .[] | ($f[.] // [])[] ] | local_edges($f))) | unique);
        .packages[]
        | select($crate == "" or .name == $crate)
        | . as $p
        | (
            ( $p.dependencies[]
              | select((.kind // "normal") != "dev")
              | [$p.name, "dep", .name, (.rename // .name), (.kind // "normal"), (.target // "any"), .req,
                 ((.features // []) | sort | join(",")),
                 # Not `// true`: jq treats a false left-hand side as absent, so that
                 # spelling would report every dependency as taking the defaults.
                 (if .uses_default_features == false then "no-defaults" else "defaults" end)] ),
            ( ($p.features // {}) as $f
              | default_closure($f) as $d
              | ($f | keys[]) as $n
              | select($n != "default")
              | [$p.name, "feature", $n, (if ($d | index($n)) then "default" else "optional" end)] )
          )
        | @tsv' <<< "$meta"
}

# Echo one row per dependency requirement floor that rose between the fact sets $1
# (baseline) and $2, tab-separated:
#
#   <crate> <label> <kind> <old req> <new req>
#
# Both arguments are manifest_facts_at_rev output, so a whole-workspace set and a
# single-crate one read alike: pass 2b below judges one crate with it, --list-raised-floors
# the whole workspace.
#
# A floor *rose* exactly when the lowest version the requirement admits went up. A
# dependency added or removed, an alias changed, a bound widened or lowered, and a
# requirement req_min cannot parse are all deliberately none of that: they are changes a
# consumer can see, but not ones that stop it resolving the crate, so this pass does not
# report them and semver-level leaves them at patch.
raised_floors() {
    local base_facts=$1 now_facts=$2
    local base_reqs now_reqs
    # The projection stops at the requirement: the feature columns after it are
    # selection facts (see manifest_facts_at_rev), and folding them into the key would
    # make a dependency that only changed features look absent from the baseline.
    base_reqs=$(awk -F'\t' '$2 == "dep" { print $1 "\t" $3 "\t" $4 "\t" $5 "\t" $6 "\t" $7 }' <<< "$base_facts")
    now_reqs=$(awk -F'\t' '$2 == "dep" { print $1 "\t" $3 "\t" $4 "\t" $5 "\t" $6 "\t" $7 }' <<< "$now_facts")

    local crate dep_name dep_alias dep_kind dep_target dep_req dep_label old_req old_min new_min
    while IFS=$'\t' read -r crate dep_name dep_alias dep_kind dep_target dep_req; do
        [[ -z "$dep_name" ]] && continue
        # Crate, name, alias, kind and target together: see manifest_facts_at_rev for
        # why the alias belongs in the key.
        old_req=$(awk -F'\t' -v c="$crate" -v n="$dep_name" -v a="$dep_alias" -v k="$dep_kind" -v t="$dep_target" \
            '$1 == c && $2 == n && $3 == a && $4 == k && $5 == t { print $6; exit }' <<< "$base_reqs")
        # Absent from the baseline: a dependency newly added, or one whose alias
        # changed. Either way not a raised floor.
        [[ -z "$old_req" ]] && continue
        [[ "$old_req" == "$dep_req" ]] && continue

        dep_label="$dep_name"
        [[ "$dep_alias" != "$dep_name" ]] && dep_label="$dep_name as $dep_alias"

        old_min=$(req_min "$old_req")
        new_min=$(req_min "$dep_req")
        if [[ -z "$old_min" || -z "$new_min" ]]; then
            log_verbose "Not judging $crate's $dep_label: unparsed requirement ($old_req -> $dep_req)"
            continue
        fi
        if version_gt "$new_min" "$old_min"; then
            printf '%s\t%s\t%s\t%s\t%s\n' "$crate" "$dep_label" "$dep_kind" "$old_req" "$dep_req"
        fi
    done <<< "$now_reqs"
}

# Echo the floors that rose for every workspace member between revisions $1 and $2, in
# raised_floors' format. One comparison of the two revisions, not a walk of the commits
# between them: a raise later reverted, or a bound lowered and raised back, nets out to
# nothing here exactly as it does for the crate's next release.
list_raised_floors() {
    local baseline=$1 current=$2
    local base_facts now_facts
    if ! base_facts=$(manifest_facts_at_rev "" "$baseline"); then
        return 1
    fi
    if ! now_facts=$(manifest_facts_at_rev "" "$current"); then
        return 1
    fi
    raised_floors "$base_facts" "$now_facts"
}

# Echo the name of every workspace member whose manifest facts moved between revisions
# $1 and $2, one per line, so a caller can run the passes below on each.
#
# This exists because the facts are *resolved* ones: an edit to the root manifest's
# [workspace.dependencies] raises the floor of every member that inherits the entry
# while touching no file under any member's directory. A caller selecting crates by
# changed file path sees nothing to check and would score such a PR as no change at all.
#
# Publishability is the caller's business: a member it does not release never comes up.
list_affected_crates() {
    local baseline=$1 current=$2
    local base_facts now_facts
    if ! base_facts=$(manifest_facts_at_rev "" "$baseline"); then
        return 1
    fi
    if ! now_facts=$(manifest_facts_at_rev "" "$current"); then
        return 1
    fi
    # A row present in exactly one of the two revisions is a fact that moved, which
    # `uniq -u` keeps and the unchanged pairs it drops. A member added or removed
    # between the revisions has every row on one side only, so it reads as affected.
    printf '%s\n%s\n' "$base_facts" "$now_facts" \
        | grep -v '^$' \
        | sort \
        | uniq -u \
        | cut -f1 \
        | sort -u
}

# version_gt A B — true when the dotted numeric triple A is strictly greater than B.
# Compared in the shell rather than with `sort -V`, a GNU extension.
version_gt() {
    local -a a=() b=()
    local i x y
    IFS='.' read -r -a a <<< "$1"
    IFS='.' read -r -a b <<< "$2"
    for i in 0 1 2; do
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

# Echo the lowest version requirement $1 admits, as `X.Y.Z`. The report quotes
# requirements verbatim; this is for ordering only.
#
# An EXCLUSIVE bound is advanced to the first version it does admit, because `>`
# excludes everything up to and including the components it states: `>1.2.3` starts at
# 1.2.4, `>1.2` at 1.3.0 (no 1.2.x matches at all) and `>1` at 2.0.0. Reading the stated
# version as the floor instead would make `>=1.2.1` -> `>1.2` look like a lowering
# rather than the raise it is, and `>1.2.3` -> the equivalent `>=1.2.4` look like a
# raise.
#
# Echo nothing when there is no lower bound (`*`, `<2`) or it cannot be parsed: callers
# treat that as "no opinion", so an exotic requirement can never invent a bump. A
# pre-release bound is compared as its release version (`1.0.0-rc.1` -> `1.0.0`), which
# can only understate a raise; an exclusive one is then left unadvanced, since
# `>1.2.3-rc.1` admits 1.2.3 itself and that release version is already its exact floor.
# A wildcard is treated the same way, `>` not being able to carry one legally.
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
        # The two spellings whose release version is already the exact floor, so that
        # advancing below would overstate the requirement: see the note above.
        [[ "$bound" == *-* || "$bound" == *'*'* ]] && exclusive=0
        bound=${bound%%[-+]*}   # drop pre-release and build metadata
        bound=${bound//\*/0}    # `1.*` admits 1.0.0
        [[ "$bound" =~ ^[0-9]+(\.[0-9]+)*$ ]] || continue
        IFS='.' read -r -a v <<< "$bound"
        if (( exclusive )); then
            # Advance the last component the bound states; the ones after it are zero
            # already. 10# so that a zero-padded component is not read as octal.
            if (( ${#v[@]} == 1 )); then
                v=( "$(( 10#${v[0]} + 1 ))" 0 0 )
            elif (( ${#v[@]} == 2 )); then
                v=( "${v[0]}" "$(( 10#${v[1]} + 1 ))" 0 )
            else
                v=( "${v[0]}" "${v[1]}" "$(( 10#${v[2]} + 1 ))" )
            fi
        fi
        bound="${v[0]:-0}.${v[1]:-0}.${v[2]:-0}"
        if [[ -z "$best" ]] || version_gt "$bound" "$best"; then
            best=$bound
        fi
    done
    if [[ -n "$best" ]]; then
        printf '%s\n' "$best"
    fi
}

# Fetch baseline ref $1 and echo the revision to read it by. Shared so that both entry
# points resolve a caller's ref identically.
resolve_baseline() {
    local baseline=$1

    # A full commit SHA already in this clone is taken as it stands: it is not a ref name
    # the remote would serve by name, and `origin/<sha>` is not a revision. Narrow on
    # purpose -- a branch or tag still resolves through the fetch below, so a stale local
    # copy can never stand in for the remote's.
    if [[ "$baseline" =~ ^[0-9a-f]{40}$ ]] && git rev-parse --verify --quiet "${baseline}^{commit}" >/dev/null; then
        printf '%s\n' "$baseline"
        return 0
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
    printf '%s\n' "$baseline"
}

compute_semver_results() {
    local crate=$1
    local baseline=$2
    local current=$3

    # If current is not provided set it to the tip of the branch
    if [ -z "$current" ]; then
        current="HEAD"
    fi

    local resolve_status
    baseline=$(resolve_baseline "$baseline")
    resolve_status=$?
    if [[ $resolve_status -ne 0 ]]; then
        return "$resolve_status"
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
        if ! base_facts=$(manifest_facts_at_rev "$crate" "$baseline"); then
            exit 1
        fi
        if ! now_facts=$(manifest_facts_at_rev "$crate" "$current"); then
            exit 1
        fi
        manifest_compared=true
    fi

    if $manifest_compared && [[ "$level_so_far" == "minor" ]]; then
        log_verbose "Skipping dependency requirement diff: already at minor, which is this pass's ceiling"
    elif $manifest_compared; then
        local raised
        # Field 1 is the crate, identical on every row here, so it is dropped again.
        raised=$(raised_floors "$base_facts" "$now_facts" \
            | awk -F'\t' '{ printf "%s (%s): %s -> %s\n", $2, $3, $4, $5 }')

        if [[ -n "$raised" ]]; then
            dep_level="minor"
            dep_reason="Dependency requirement floor raised"
            dep_details=$(truncate_details 50 <<< "$raised")
            log_verbose "floors raised:"$'\n'"$raised"
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
    # Not scored for its own sake: a change to what a feature *enables* — the two
    # *_enables_feature lints' job. It reaches this pass only where it moves the set of
    # features `default` reaches, which the rows resolve (see manifest_facts_at_rev);
    # emptying a `bundle` that `default` lists undefaults everything below it.
    # ----------------------------------------------------------------
    if $manifest_compared; then
        local base_features now_features
        local feat_name feat_default removed="" added="" undefaulted=""
        base_features=$(awk -F'\t' '$2 == "feature" { print $3 "\t" $4 }' <<< "$base_facts")
        now_features=$(awk -F'\t' '$2 == "feature" { print $3 "\t" $4 }' <<< "$now_facts")

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

# The list modes stop here: they report on the workspace rather than checking one crate,
# so none of the per-crate machinery below runs.
if [[ -n "$LIST_MODE" ]]; then
    # A baseline named as a ref is compared from where the branch left it, the way a
    # changed-file search uses `git diff base...HEAD`: a fact that moved on the baseline
    # *since* then is not this branch's doing, and reporting it would put another
    # branch's change on this branch's report.
    #
    # A baseline given as a full commit SHA is compared from as given, the caller having
    # resolved it already. The two agree wherever that commit is an ancestor of
    # CURRENT_REF, since it is then its own merge base; they part where it is not, and
    # there only the caller knows which it meant. A release tag that a squash merge left
    # off HEAD's history is exactly that case: its merge base predates the release, so
    # floors measured from there include ones the released version already states.
    BASE_GIVEN_AS_COMMIT=false
    if [[ "$BASE_REF" =~ ^[0-9a-f]{40}$ ]]; then
        BASE_GIVEN_AS_COMMIT=true
    fi
    if ! BASE_REF=$(resolve_baseline "$BASE_REF"); then
        exit 1
    fi
    if $BASE_GIVEN_AS_COMMIT; then
        FORK_POINT="$BASE_REF"
    else
        # A clone too shallow to hold a merge base falls back to the baseline tip,
        # erring towards reporting too much.
        FORK_POINT=$(git merge-base "$BASE_REF" "$CURRENT_REF" 2>/dev/null)
        if [[ -z "$FORK_POINT" ]]; then
            echo "Warning: no merge base for $BASE_REF and $CURRENT_REF; comparing against the baseline tip" >&2
            FORK_POINT="$BASE_REF"
        fi
    fi
    case "$LIST_MODE" in
        affected)
            log_verbose "Listing crates whose manifest facts moved between $FORK_POINT and $CURRENT_REF"
            list_affected_crates "$FORK_POINT" "$CURRENT_REF"
            ;;
        raised-floors)
            log_verbose "Listing floors raised between $FORK_POINT and $CURRENT_REF"
            list_raised_floors "$FORK_POINT" "$CURRENT_REF"
            ;;
    esac
    exit $?
fi

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
