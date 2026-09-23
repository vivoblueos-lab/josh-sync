#!/usr/bin/env bash
set -euo pipefail

gh label create "$PR_LABEL" \
  --color '1D76DB' \
  --description 'Pull request managed by josh-sync' \
  --force

# Check if an open pull request already exists
RESULT="$(gh pr list \
  --author "$PR_AUTHOR" \
  --state open \
  --json title \
  --jq 'map(select(.title=="BlueOS pull update")) | length')"
if [ "$RESULT" -eq 0 ]; then
  echo "Creating new pull request"
  PR_URL="$(gh pr create \
    -B "$PR_BASE_BRANCH" \
    --title 'BlueOS pull update' \
    --body 'Latest update from the BlueOS monorepo.' \
    --label "$PR_LABEL")"
  echo "Created pull request ${PR_URL}"
  echo "pr_url=$PR_URL" >> "$GITHUB_OUTPUT"
else
  PR_URL="$(gh pr list \
    --author "$PR_AUTHOR" \
    --state open \
    --json url,title \
    --jq 'map(select(.title=="BlueOS pull update")) | .[0].url')"
  PR_NUMBER="${PR_URL##*/}"
  jq -n --arg label "$PR_LABEL" '{labels: [$label]}' | \
    gh api \
      --method POST \
      "repos/$GITHUB_REPOSITORY/issues/$PR_NUMBER/labels" \
      --input - >/dev/null
  echo "Updating pull request ${PR_URL}"
  echo "pr_url=$PR_URL" >> "$GITHUB_OUTPUT"
fi

if [[ "${APPROVAL_APP_ENABLED:-false}" == "true" ]]; then
  PR_NUMBER="${PR_URL##*/}"
  PR_HEAD_SHA="$(gh pr view "$PR_URL" \
    --json headRefOid \
    --jq .headRefOid)"

  GH_TOKEN="$APPROVAL_GITHUB_TOKEN" gh api \
    "repos/$GITHUB_REPOSITORY/pulls/$PR_NUMBER/reviews" \
    --field event=APPROVE \
    --field commit_id="$PR_HEAD_SHA"

  echo "Approved synchronization pull request ${PR_URL}"
fi

bash "$(dirname "$0")/blueos-enable-auto-merge.sh" "$PR_URL"
