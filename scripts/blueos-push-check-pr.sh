#!/usr/bin/env bash
set -euo pipefail

BASE="$(jq -rn --arg branch "$UPSTREAM_BRANCH" '$branch | @uri')"
HEAD="$(jq -rn --arg branch "$BRANCH" '$branch | @uri')"
COMPARISON_FILE="$RUNNER_TEMP/blueos-push-comparison.json"

# The first page lists up to 300 files for the entire comparison, independently
# of commit pagination. Larger diffs cannot satisfy the single-file check.
gh api --method GET "repos/$UPSTREAM/compare/$BASE...$HEAD" \
  -f per_page=1 -f page=1 > "$COMPARISON_FILE"

SHOULD_CREATE_PR="$(jq -r '
  if (.files | type) != "array" then
    error("comparison response must contain a files array")
  else
    # Count both paths of a rename so renaming a business file to blueos-version
    # cannot hide that change.
    [.files[] | .filename, (.previous_filename // empty)] | unique |
    if length == 1 and (.[0] | split("/") | last) == "blueos-version" then
      "false"
    else
      "true"
    end
  end
' "$COMPARISON_FILE")"

if [ "$SHOULD_CREATE_PR" = false ]; then
  echo "Skipping monorepo pull request: only blueos-version changed."
fi
echo "should_create_pr=$SHOULD_CREATE_PR" >> "$GITHUB_OUTPUT"
