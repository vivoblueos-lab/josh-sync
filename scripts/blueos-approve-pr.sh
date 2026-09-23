#!/usr/bin/env bash
set -euo pipefail

PR_URL="$1"
TARGET_REPOSITORY="$2"
HEAD_BRANCH="$3"
BASE_BRANCH="$4"
PR_AUTHOR="$5"

if [[ -z "${APPROVAL_GITHUB_TOKEN:-}" ]]; then
  echo 'Missing approval token' >&2
  exit 1
fi

approval_api() {
  GH_TOKEN="$APPROVAL_GITHUB_TOKEN" gh api "$@"
}

PR_NUMBER="${PR_URL##*/}"
if [[ ! "$PR_NUMBER" =~ ^[0-9]+$ || \
      "$PR_URL" != "https://github.com/$TARGET_REPOSITORY/pull/$PR_NUMBER" ]]; then
  echo "Unexpected synchronization PR URL: $PR_URL" >&2
  exit 1
fi

APPROVAL_LOGIN="$(approval_api user --jq '.login // empty')"
if [[ -z "$APPROVAL_LOGIN" ]]; then
  echo 'Could not identify approval token owner' >&2
  exit 1
fi

echo "Checking synchronization PR $PR_URL"
PR_JSON="$(approval_api "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER")"
if ! jq -e \
  --arg repo "$TARGET_REPOSITORY" \
  --arg head "$HEAD_BRANCH" \
  --arg base "$BASE_BRANCH" \
  --arg author "$PR_AUTHOR" \
  --arg reviewer "$APPROVAL_LOGIN" \
  '.state == "open" and .draft == false and
   .base.repo.full_name == $repo and .base.ref == $base and
   .head.repo.full_name == $repo and .head.ref == $head and
   .user.login == $author and .user.login != $reviewer' <<< "$PR_JSON" >/dev/null; then
  echo "Refusing to approve unexpected pull request: $PR_URL" >&2
  exit 1
fi
HEAD_SHA="$(jq -r '.head.sha' <<< "$PR_JSON")"
REVIEWS="$(approval_api --method GET -f per_page=100 \
  "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER/reviews")"
if jq -e --arg sha "$HEAD_SHA" --arg reviewer "$APPROVAL_LOGIN" \
  'any(.state == "APPROVED" and .commit_id == $sha and .user.login == $reviewer)' \
  <<< "$REVIEWS" >/dev/null; then
  echo "Current head of $PR_URL already meets the approval rule"
  exit 0
fi

echo "Approving current head $HEAD_SHA"
REVIEW="$(approval_api --method POST -f event=APPROVE \
  -f "commit_id=$HEAD_SHA" \
  "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER/reviews")"
if ! jq -e --arg author "$PR_AUTHOR" --arg sha "$HEAD_SHA" \
  --arg reviewer "$APPROVAL_LOGIN" \
  '.state == "APPROVED" and .commit_id == $sha and
   .user.login == $reviewer and .user.login != $author' \
  <<< "$REVIEW" >/dev/null; then
  echo "Approval API did not approve the current head of $PR_URL" >&2
  exit 1
fi
REVIEWER="$(jq -r '.user.login' <<< "$REVIEW")"
echo "Approved $PR_URL at $HEAD_SHA as $REVIEWER"
