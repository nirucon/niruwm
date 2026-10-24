#!/bin/sh
# A release must be built from a tag, never from a branch.
#
# v0.16.64 shipped assets built from `main` while its tag pointed at another
# commit, because the only version check ran under `if [[ "$GITHUB_REF" ==
# refs/tags/* ]]` and was therefore skipped for a manual run. This test states
# the invariant directly so the guard cannot silently disappear again.
set -eu
SOURCE=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
WORKFLOW="$SOURCE/.github/workflows/release.yml"
[ -f "$WORKFLOW" ] || { printf 'missing %s\n' "$WORKFLOW" >&2; exit 1; }

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# A tag push must publish that tag.
grep -q 'tags: \["v\*"\]' "$WORKFLOW" ||
  fail "the release must trigger on v* tags"

# The checkout must be pinned to the tag, not the default ref.
grep -q 'ref: \${{ env.TAG }}' "$WORKFLOW" ||
  fail "checkout is not pinned to the tag (ref: env.TAG)"

# A manual run must at least name a tag and be validated.
if grep -q 'workflow_dispatch' "$WORKFLOW"; then
  grep -q 'existing tag to rebuild' "$WORKFLOW" ||
    fail "workflow_dispatch must be documented as a tag rebuild only"
  grep -q 'Refuse anything that is not a tag' "$WORKFLOW" ||
    fail "workflow_dispatch has no tag guard"
  # The guard must run for both event kinds, not only inside a tags/* test.
  if grep -q 'if \[\[ "\$GITHUB_REF" == refs/tags/\* \]\]' "$WORKFLOW"; then
    fail "the old skipped-outside-tag version check is back"
  fi
fi

# The version in Cargo.toml must match the tag being released.
grep -q 'Cargo.toml says' "$WORKFLOW" ||
  fail "no guard that the tag matches Cargo.toml"

# The published tag comes from the verified TAG, not from a recomputed value.
grep -q 'RELEASE_TAG=\$TAG' "$WORKFLOW" ||
  fail "the published tag does not use the verified TAG"

printf 'release-source: ok\n'
