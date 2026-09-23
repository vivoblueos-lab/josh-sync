#!/usr/bin/env bash
set -euo pipefail

PR_URL="$1"
TARGET_REPOSITORY="$2"
HEAD_BRANCH="$3"
BASE_BRANCH="$4"
PR_AUTHOR="$5"

if [[ -z "${SYNC_GITHUB_TOKEN:-}" || -z "${APPROVAL_GITHUB_TOKEN:-}" ||
      -z "${APPROVAL_UNSUBSCRIBE_GITHUB_TOKEN:-}" ]]; then
  echo 'Missing synchronization, approval, or unsubscribe token' >&2
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
PR_NODE_ID="$(jq -r '.node_id' <<< "$PR_JSON")"
API_URL="${GITHUB_API_URL:-https://api.github.com}"
UNSUBSCRIBE_LOGIN="$(curl -fsS \
  -H "Authorization: Bearer $APPROVAL_UNSUBSCRIBE_GITHUB_TOKEN" \
  -H 'Accept: application/vnd.github+json' \
  "$API_URL/user" | jq -r '.login // empty')"
if [[ -z "$UNSUBSCRIBE_LOGIN" ]]; then
  echo 'Could not identify unsubscribe token owner' >&2
  exit 1
fi

unsubscribe_approval_user() {
  local response notifications thread_id
  response="$(jq -n --arg id "$PR_NODE_ID" \
    '{query:"mutation($id:ID!){updateSubscription(input:{subscribableId:$id,state:UNSUBSCRIBED}){subscribable{viewerSubscription}}}",variables:{id:$id}}' | \
    curl -fsS \
      -X POST \
      -H "Authorization: Bearer $APPROVAL_UNSUBSCRIBE_GITHUB_TOKEN" \
      -H 'Accept: application/vnd.github+json' \
      -H 'Content-Type: application/json' \
      --data-binary @- \
      "$API_URL/graphql")"
  if jq -e '.errors == null and
    .data.updateSubscription.subscribable.viewerSubscription == "UNSUBSCRIBED"' \
    <<< "$response" >/dev/null; then
    echo "Unsubscribed $UNSUBSCRIBE_LOGIN from $PR_URL"
    return
  fi

  echo 'GraphQL unsubscribe unavailable; trying notification thread API'
  for attempt in {1..6}; do
    notifications="$(curl -fsS \
      -H "Authorization: Bearer $APPROVAL_UNSUBSCRIBE_GITHUB_TOKEN" \
      -H 'Accept: application/vnd.github+json' \
      "$API_URL/repos/$TARGET_REPOSITORY/notifications?all=true&per_page=100")"
    thread_id="$(jq -r --arg url "$API_URL/repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER" \
      '[.[] | select(.subject.url == $url) | .id] | first // empty' \
      <<< "$notifications")"
    if [[ -n "$thread_id" ]]; then
      curl -fsS \
        -X DELETE \
        -H "Authorization: Bearer $APPROVAL_UNSUBSCRIBE_GITHUB_TOKEN" \
        -H 'Accept: application/vnd.github+json' \
        "$API_URL/notifications/threads/$thread_id/subscription" >/dev/null
      echo "Unsubscribed $UNSUBSCRIBE_LOGIN from $PR_URL"
      return
    fi
    if [[ "$attempt" -lt 6 ]]; then
      sleep 2
    fi
  done
  echo "No notification thread found for $PR_URL; could not unsubscribe $UNSUBSCRIBE_LOGIN" >&2
  exit 1
}

REVIEW_DECISION="$(GH_TOKEN="$SYNC_GITHUB_TOKEN" gh pr view "$PR_URL" \
  --json reviewDecision --jq '.reviewDecision')"
if [[ "$REVIEW_DECISION" == APPROVED ]]; then
  REVIEWS="$(GH_TOKEN="$SYNC_GITHUB_TOKEN" gh api \
    --method GET -f per_page=100 \
    "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER/reviews")"
  if jq -e --arg sha "$HEAD_SHA" --arg reviewer "$UNSUBSCRIBE_LOGIN" \
    'any(.state == "APPROVED" and .commit_id == $sha and .user.login == $reviewer)' \
    <<< "$REVIEWS" >/dev/null; then
    echo "Current head of $PR_URL already meets the approval rule"
    unsubscribe_approval_user
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
  --arg reviewer "$UNSUBSCRIBE_LOGIN" \
  '.state == "APPROVED" and .commit_id == $sha and
   .user.login == $reviewer and .user.login != $author' \
  <<< "$REVIEW" >/dev/null; then
  echo "Approval API did not approve the current head of $PR_URL" >&2
  exit 1
fi
REVIEWER="$(jq -r '.user.login' <<< "$REVIEW")"
echo "Approved $PR_URL at $HEAD_SHA as $REVIEWER"
unsubscribe_approval_user
