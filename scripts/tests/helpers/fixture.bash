#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Builds a synthetic cargo workspace inside a throwaway git repository so the
# release scripts can be exercised against real `cargo metadata` output and real
# git history, without touching the libdatadog checkout.
#
# The fixture is intentionally small but covers the shapes the release scripts
# have to reason about:
#
#   libdd-alpha    1.2.3   no workspace deps; dev-depends on libdd-gamma
#                          (a dev-dependency CYCLE — publication order must ignore it)
#   libdd-beta     0.5.0   depends on libdd-alpha
#   libdd-gamma    2.0.1   depends on libdd-alpha and libdd-beta
#   libdd-delta    0.1.0   build-depends on libdd-beta, dev-depends on libdd-alpha
#                          (kind filters: build counts for publication order, dev never does;
#                           major-bumps-level.sh must ignore both)
#   libdd-private  0.9.0   publish = false (must be excluded unless asked for)
#   other-tool     0.3.0   depends on libdd-alpha but is not libdd-* itself
#                          (major-bumps-level.sh only tracks libdd-* dependencies)

# Offline everywhere: the fixture has path dependencies only, so cargo never needs
# the network, and an accidental registry hit would make the suite flaky.
export CARGO_NET_OFFLINE=true

# fixture_init [DIR] — create the workspace and an initial commit. Leaves the shell
# in the repository root and exports FIXTURE_REPO.
fixture_init() {
  FIXTURE_REPO="${1:-${TEST_TMP}/workspace}"
  export FIXTURE_REPO
  mkdir -p "$FIXTURE_REPO"

  cat > "${FIXTURE_REPO}/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = [
    "libdd-alpha",
    "libdd-beta",
    "libdd-gamma",
    "libdd-delta",
    "libdd-private",
    "other-tool",
]
EOF

  _fixture_crate libdd-alpha 1.2.3
  cat >> "${FIXTURE_REPO}/libdd-alpha/Cargo.toml" <<'EOF'

[dev-dependencies]
libdd-gamma = { path = "../libdd-gamma", version = "2.0" }
EOF

  _fixture_crate libdd-beta 0.5.0
  cat >> "${FIXTURE_REPO}/libdd-beta/Cargo.toml" <<'EOF'

[dependencies]
libdd-alpha = { path = "../libdd-alpha", version = "1.2" }
EOF

  _fixture_crate libdd-gamma 2.0.1
  cat >> "${FIXTURE_REPO}/libdd-gamma/Cargo.toml" <<'EOF'

[dependencies]
libdd-alpha = { path = "../libdd-alpha", version = "1.2" }
libdd-beta = { path = "../libdd-beta", version = "0.5" }
EOF

  _fixture_crate libdd-delta 0.1.0
  cat >> "${FIXTURE_REPO}/libdd-delta/Cargo.toml" <<'EOF'

[build-dependencies]
libdd-beta = { path = "../libdd-beta", version = "0.5" }

[dev-dependencies]
libdd-alpha = { path = "../libdd-alpha", version = "1.2" }
EOF
  # A build script is not needed for cargo metadata, but keep the manifest honest.
  printf 'fn main() {}\n' > "${FIXTURE_REPO}/libdd-delta/build.rs"

  _fixture_crate libdd-private 0.9.0 'publish = false'
  cat >> "${FIXTURE_REPO}/libdd-private/Cargo.toml" <<'EOF'

[dependencies]
libdd-alpha = { path = "../libdd-alpha", version = "1.2" }
EOF

  _fixture_crate other-tool 0.3.0
  cat >> "${FIXTURE_REPO}/other-tool/Cargo.toml" <<'EOF'

[dependencies]
libdd-alpha = { path = "../libdd-alpha", version = "1.2" }
EOF

  cd "$FIXTURE_REPO" || return 1
  git init -q -b main .
  git config user.name "Fixture Author"
  git config user.email "fixture@example.com"
  git config commit.gpgsign false
  git config tag.gpgsign false
  fixture_commit_all "chore: initial workspace"
}

_fixture_crate() {
  local name="$1" version="$2" extra="${3:-}"
  mkdir -p "${FIXTURE_REPO}/${name}/src"
  {
    printf '[package]\n'
    printf 'name = "%s"\n' "$name"
    printf 'version = "%s"\n' "$version"
    printf 'edition = "2021"\n'
    [[ -n "$extra" ]] && printf '%s\n' "$extra"
  } > "${FIXTURE_REPO}/${name}/Cargo.toml"
  printf '// %s\n' "$name" > "${FIXTURE_REPO}/${name}/src/lib.rs"
}

# fixture_commit_all MESSAGE [AUTHOR_NAME] [AUTHOR_EMAIL]
fixture_commit_all() {
  local message="$1" author="${2:-Fixture Author}" email="${3:-fixture@example.com}"
  git add -A
  GIT_AUTHOR_NAME="$author" GIT_AUTHOR_EMAIL="$email" \
    GIT_COMMITTER_NAME="Fixture Author" GIT_COMMITTER_EMAIL="fixture@example.com" \
    git commit -q --allow-empty -m "$message"
}

# fixture_touch_crate CRATE MESSAGE [AUTHOR_NAME] [AUTHOR_EMAIL]
# Make a commit that changes only CRATE's directory, so path-filtered git log
# picks it up for that crate and no other.
fixture_touch_crate() {
  local crate="$1" message="$2" author="${3:-Fixture Author}" email="${4:-fixture@example.com}"
  printf '// %s\n' "$message" >> "${FIXTURE_REPO}/${crate}/src/lib.rs"
  fixture_commit_all "$message" "$author" "$email"
}

# fixture_set_dep_req CRATE DEP REQ — rewrite a dependency's version requirement.
fixture_set_dep_req() {
  local crate="$1" dep="$2" req="$3"
  sed -i -E "s|^(${dep} = \{ path = \"[^\"]*\", version = )\"[^\"]*\"|\1\"${req}\"|" \
    "${FIXTURE_REPO}/${crate}/Cargo.toml"
}

# fixture_tag TAG [--annotated] — tag HEAD.
fixture_tag() {
  local tag="$1" kind="${2:-}"
  if [[ "$kind" == "--annotated" ]]; then
    git tag -a "$tag" -m "release $tag"
  else
    git tag "$tag"
  fi
}

# fixture_add_origin — publish the fixture to a bare repo and wire it as `origin`,
# for scripts that run `git fetch origin` or resolve `origin/<branch>`.
fixture_add_origin() {
  local bare="${TEST_TMP}/origin.git"
  git init -q --bare "$bare"
  git remote add origin "$bare"
  git push -q origin main --tags
  git fetch -q origin
}

# --- topological-order assertion --------------------------------------------

# assert_topological_order ORDER_LINES DEPS_SPEC
# ORDER_LINES: newline-separated crate names, in claimed publication order.
# DEPS_SPEC:   newline-separated "crate:dep1,dep2" entries.
#
# Asserting the exact list would pin Kahn's tie-breaking, which is an
# implementation detail. Assert the property the workflow actually relies on:
# every crate is published after all of its workspace dependencies.
assert_topological_order() {
  local order="$1" deps_spec="$2"
  local -a seen=()
  local line crate dep spec_crate spec_deps found

  while IFS= read -r crate; do
    [[ -z "$crate" ]] && continue
    while IFS= read -r line; do
      [[ -z "$line" ]] && continue
      spec_crate="${line%%:*}"
      [[ "$spec_crate" != "$crate" ]] && continue
      spec_deps="${line#*:}"
      while IFS= read -r dep; do
        [[ -z "$dep" ]] && continue
        found=false
        for s in "${seen[@]:-}"; do
          [[ "$s" == "$dep" ]] && found=true && break
        done
        if [[ "$found" != true ]]; then
          fail_with "publication order violated: '$crate' is listed before its dependency '$dep'" \
            "--- order ---" "$order"
          return 1
        fi
      done < <(tr ',' '\n' <<< "$spec_deps")
    done <<< "$deps_spec"
    seen+=("$crate")
  done <<< "$order"
}
