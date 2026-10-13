#!/usr/bin/env bash
set -euo pipefail

CREDENTIALS_FILE="$RUNNER_TEMP/josh-sync-credentials"
git config --global credential.helper "store --file=$CREDENTIALS_FILE"
printf '%s\n' \
  'protocol=https' \
  'host=github.com' \
  'username=x-access-token' \
  "password=$APP_TOKEN" \
  '' | git credential approve
printf '%s\n' \
  'protocol=http' \
  'host=localhost:42042' \
  'username=x-access-token' \
  "password=$APP_TOKEN" \
  '' | git credential approve
