#!/usr/bin/env bash
set -euo pipefail

PR_URL="$1"

case "${JOSH_SYNC_AUTO_MERGE:-false}" in
  true|TRUE|True)
    ;;
  false|FALSE|False|"")
    exit 0
    ;;
  *)
    echo "Invalid JOSH_SYNC_AUTO_MERGE value: ${JOSH_SYNC_AUTO_MERGE}" >&2
    exit 1
    ;;
esac

if gh pr merge "$PR_URL" --auto --merge; then
  echo "Enabled auto-merge for ${PR_URL}"
  exit 0
fi

# A conflicting synchronization PR is expected to remain open until it is
# regenerated or resolved. Preserve that state while surfacing other errors.
MERGE_STATE="$(gh pr view "$PR_URL" \
  --json mergeable,mergeStateStatus \
  --jq '[.mergeable, .mergeStateStatus] | @tsv' 2>/dev/null || true)"
if [[ "$MERGE_STATE" == CONFLICTING$'\t'* || "$MERGE_STATE" == *$'\t'DIRTY ]]; then
  echo "Synchronization PR ${PR_URL} has conflicts; leaving it open."
  exit 0
fi

echo "Failed to enable auto-merge for ${PR_URL}" >&2
exit 1
