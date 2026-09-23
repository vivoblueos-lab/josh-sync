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

echo "Checking synchronization PR $PR_URL"
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
API_URL="${GITHUB_API_URL:-https://api.github.com}"
REVIEW_DECISION="$(GH_TOKEN="$SYNC_GITHUB_TOKEN" gh pr view "$PR_URL" \
  --json reviewDecision --jq '.reviewDecision')"
if [[ "$REVIEW_DECISION" == APPROVED ]]; then
  REVIEWS="$(GH_TOKEN="$SYNC_GITHUB_TOKEN" gh api \
    --method GET -f per_page=100 \
    "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER/reviews")"
  if jq -e --arg sha "$HEAD_SHA" \
    'any(.state == "APPROVED" and .commit_id == $sha)' \
    <<< "$REVIEWS" >/dev/null; then
    echo "Current head of $PR_URL already meets the approval rule"
    exit 0
  fi
fi

echo "Approving current head $HEAD_SHA"
REVIEW="$(jq -n --arg sha "$HEAD_SHA" \
  '{event:"APPROVE",commit_id:$sha}' | curl -fsS \
  -X POST \
  -H "Authorization: Bearer $APPROVAL_GITHUB_TOKEN" \
  -H 'Accept: application/vnd.github+json' \
  -H 'Content-Type: application/json' \
  --data-binary @- \
  "$API_URL/repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER/reviews")"
if ! jq -e --arg author "$PR_AUTHOR" --arg sha "$HEAD_SHA" \
  '.state == "APPROVED" and .commit_id == $sha and
   (.user.login | type == "string" and . != $author)' \
  <<< "$REVIEW" >/dev/null; then
  echo "Approval API did not approve the current head of $PR_URL" >&2
  exit 1
fi
REVIEWER="$(jq -r '.user.login' <<< "$REVIEW")"
echo "Approved $PR_URL at $HEAD_SHA as $REVIEWER"
