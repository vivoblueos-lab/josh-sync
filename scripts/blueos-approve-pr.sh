#!/usr/bin/env bash
set -euo pipefail

PR_URL="$1"
TARGET_REPOSITORY="$2"
HEAD_BRANCH="$3"
BASE_BRANCH="$4"
PR_AUTHOR="$5"

if [[ -z "${SYNC_GITHUB_TOKEN:-}" || -z "${APPROVAL_GITHUB_TOKEN:-}" ]]; then
  echo 'Missing synchronization or approval token' >&2
  exit 1
fi
if [[ "$SYNC_GITHUB_TOKEN" == "$APPROVAL_GITHUB_TOKEN" ]]; then
  echo 'Approval token must differ from the PR creator token' >&2
  exit 1
fi

PR_NUMBER="${PR_URL##*/}"
if [[ ! "$PR_NUMBER" =~ ^[0-9]+$ || \
      "$PR_URL" != "https://github.com/$TARGET_REPOSITORY/pull/$PR_NUMBER" ]]; then
  echo "Unexpected synchronization PR URL: $PR_URL" >&2
  exit 1
fi

PR_JSON="$(GH_TOKEN="$SYNC_GITHUB_TOKEN" gh api \
  "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER")"
if ! jq -e \
  --arg repo "$TARGET_REPOSITORY" \
  --arg head "$HEAD_BRANCH" \
  --arg base "$BASE_BRANCH" \
  --arg author "$PR_AUTHOR" \
  '.state == "open" and .draft == false and
   .base.repo.full_name == $repo and .base.ref == $base and
   .head.repo.full_name == $repo and .head.ref == $head and
   .user.login == $author' <<< "$PR_JSON" >/dev/null; then
  echo "Refusing to approve unexpected pull request: $PR_URL" >&2
  exit 1
fi
HEAD_SHA="$(jq -r '.head.sha' <<< "$PR_JSON")"
REVIEWER="$(GH_TOKEN="$APPROVAL_GITHUB_TOKEN" gh api user --jq '.login')"
if [[ -z "$REVIEWER" || "$REVIEWER" == "$PR_AUTHOR" ]]; then
  echo 'Approval user must differ from the PR author' >&2
  exit 1
fi

REVIEWS="$(GH_TOKEN="$APPROVAL_GITHUB_TOKEN" gh api \
  --method GET --paginate -f per_page=100 \
  "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER/reviews")"
if jq -se --arg reviewer "$REVIEWER" --arg sha "$HEAD_SHA" \
  'add | any(.user.login == $reviewer and .state == "APPROVED" and .commit_id == $sha)' \
  <<< "$REVIEWS" >/dev/null; then
  echo "Current head of $PR_URL is already approved by $REVIEWER"
  exit 0
fi

GH_TOKEN="$APPROVAL_GITHUB_TOKEN" gh api \
  --method POST "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER/reviews" \
  -f event=APPROVE -f "commit_id=$HEAD_SHA" \
  --jq '{state,commit_id,user:.user.login}'
echo "Approved $PR_URL at $HEAD_SHA as $REVIEWER"
