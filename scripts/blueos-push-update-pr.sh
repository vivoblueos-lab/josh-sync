#!/usr/bin/env bash
set -euo pipefail

SOURCE_SHA="$(git rev-parse HEAD)"
TITLE="$SUBREPO subtree update"
BODY_FILE="$RUNNER_TEMP/blueos-push-pr-body.md"
cat > "$BODY_FILE" <<EOF
Subtree update of \`$SUBREPO\` to https://github.com/$GITHUB_REPOSITORY/commit/$SOURCE_SHA.

Created using vivoblueos-josh-sync.

Do NOT amend/squash/rebase any of the commits produced by this tool; that can badly break future syncs.
EOF

PULL_REQUESTS_FILE="$RUNNER_TEMP/blueos-push-prs.json"
gh api \
  --method GET \
  "repos/$UPSTREAM/pulls" \
  -f state=open \
  -f base="$UPSTREAM_BRANCH" \
  -f head="$UPSTREAM_OWNER:$BRANCH" \
  > "$PULL_REQUESTS_FILE"
mapfile -t pull_requests < <(
  jq -r '.[] | "\(.number)\t\(.html_url)"' "$PULL_REQUESTS_FILE"
)

if [ "${#pull_requests[@]}" -eq 0 ]; then
  PR_URL="$(jq -n \
    --arg title "$TITLE" \
    --arg head "$BRANCH" \
    --arg base "$UPSTREAM_BRANCH" \
    --rawfile body "$BODY_FILE" \
    '{title: $title, head: $head, base: $base, body: $body}' | \
    gh api \
      --method POST \
      "repos/$UPSTREAM/pulls" \
      --input - \
      --jq '.html_url')"
  PR_NUMBER="${PR_URL##*/}"
  echo "Created pull request $PR_URL"
elif [ "${#pull_requests[@]}" -eq 1 ]; then
  IFS=$'\t' read -r PR_NUMBER PR_URL <<< "${pull_requests[0]}"
  jq -n \
    --arg title "$TITLE" \
    --rawfile body "$BODY_FILE" \
    '{title: $title, body: $body}' | \
    gh api \
      --method PATCH \
      "repos/$UPSTREAM/pulls/$PR_NUMBER" \
      --input - >/dev/null
  echo "Updated pull request $PR_URL"
else
  echo "Found multiple open pull requests for $UPSTREAM_OWNER:$BRANCH -> $UPSTREAM_BRANCH" >&2
  exit 1
fi

gh label create "$PR_LABEL" \
  --repo "$UPSTREAM" \
  --color '1D76DB' \
  --description 'Pull request managed by josh-sync' \
  --force
jq -n --arg label "$PR_LABEL" '{labels: [$label]}' | \
  gh api \
    --method POST \
    "repos/$UPSTREAM/issues/$PR_NUMBER/labels" \
    --input - >/dev/null

bash "$(dirname "$0")/blueos-approve-pr.sh" \
  "$PR_URL" "$UPSTREAM" "$BRANCH" "$UPSTREAM_BRANCH" "$PR_AUTHOR"
bash "$(dirname "$0")/blueos-enable-auto-merge.sh" "$PR_URL"

echo "pr_url=$PR_URL" >> "$GITHUB_OUTPUT"
