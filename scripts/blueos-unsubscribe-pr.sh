#!/usr/bin/env bash
set -euo pipefail

PR_URL="$1"
TARGET_REPOSITORY="$2"
WAIT_FOR_MERGE="${3:-false}"

if [[ -z "${SYNC_GITHUB_TOKEN:-}" ||
      -z "${APPROVAL_UNSUBSCRIBE_GITHUB_TOKEN:-}" ]]; then
  echo 'Missing synchronization or unsubscribe token' >&2
  exit 1
fi

PR_NUMBER="${PR_URL##*/}"
if [[ ! "$PR_NUMBER" =~ ^[0-9]+$ ||
      "$PR_URL" != "https://github.com/$TARGET_REPOSITORY/pull/$PR_NUMBER" ]]; then
  echo "Unexpected synchronization PR URL: $PR_URL" >&2
  exit 1
fi

API_URL="${GITHUB_API_URL:-https://api.github.com}"
if [[ "$WAIT_FOR_MERGE" == after-auto-merge &&
      "${JOSH_SYNC_AUTO_MERGE:-false}" =~ ^([Tt][Rr][Uu][Ee])$ ]]; then
  for attempt in {1..12}; do
    PR_JSON="$(GH_TOKEN="$SYNC_GITHUB_TOKEN" gh api \
      "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER")"
    if jq -e '.merged_at != null or .state == "closed"' \
      <<< "$PR_JSON" >/dev/null; then
      break
    fi
    if [[ "$attempt" -lt 12 ]]; then
      sleep 5
    fi
  done
  if jq -e '.merged_at != null' <<< "$PR_JSON" >/dev/null; then
    # Merge-related notifications can recreate the reviewer's subscription.
    sleep 8
  fi
fi

PR_NODE_ID="$(GH_TOKEN="$SYNC_GITHUB_TOKEN" gh api \
  "repos/$TARGET_REPOSITORY/pulls/$PR_NUMBER" --jq '.node_id')"
UNSUBSCRIBE_LOGIN="$(curl -fsS \
  -H "Authorization: Bearer $APPROVAL_UNSUBSCRIBE_GITHUB_TOKEN" \
  -H 'Accept: application/vnd.github+json' \
  "$API_URL/user" | jq -r '.login // empty')"
if [[ -z "$UNSUBSCRIBE_LOGIN" ]]; then
  echo 'Could not identify unsubscribe token owner' >&2
  exit 1
fi

for attempt in {1..6}; do
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
    exit 0
  fi
  if [[ "$attempt" -lt 6 ]]; then
    sleep 2
  fi
done

echo "GraphQL unsubscribe unavailable for $PR_URL; trying notification thread API"
jq -c '{errorTypes:[.errors[]?.type],viewerSubscription:.data.updateSubscription.subscribable.viewerSubscription}' \
  <<< "$response"
for attempt in {1..12}; do
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
    exit 0
  fi
  if [[ "$attempt" -lt 12 ]]; then
    sleep 5
  fi
done
echo "No notification thread found for $PR_URL; could not unsubscribe $UNSUBSCRIBE_LOGIN" >&2
exit 1
