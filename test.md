# Trial test guide

This repository supports two environments without changing the synchronizer code. The selected
environment is part of the tracked `josh-sync.toml` in each subtree repository.

## Trial configuration

Start with one mapping in the Natural-selection1 trial environment:

```toml
org = "Natural-selection1"
repo = "kernel"
upstream-repo = "Natural-selection1/bluekernel-mono"
upstream-branch = "main"
path = "kernel"
filter-version = 2
```

`upstream-repo` and `upstream-branch` select the source for both directions. The reverse push
uses the repository name portion (`bluekernel-mono`) when targeting a contributor's fork.

The production migration changes the reviewed values to:

```toml
org = "vivoblueos"
upstream-repo = "vivoblueos/bluekernel"
```

Do not reuse a trial `bluekernel-version` value in production; bootstrap a new baseline from a
confirmed equal tree in the production repositories.

## Local trial sequence

1. Create the tracked mapping and `bluekernel-version` through ordinary PRs in the subtree and the
   corresponding monorepo path. Confirm the two trees have the same hash before writing the
   baseline.
2. On a throwaway subtree branch, run `vivoblueos-josh-sync pull --upstream-commit <trial-sha>`.
   Verify the preparation commit, Josh merge commit, changed path, and root-commit count. Push
   only this branch and open a normal subtree PR.
3. After that PR merges, add a small subtree-only change through a normal subtree PR. From the
   updated default branch, run `vivoblueos-josh-sync push <trial-branch> <github-user>`.
   Confirm the generated branch changes only the mapped monorepo path and that the round-trip
   check succeeds before opening the monorepo PR.
4. Run both directions again. They must be no-ops before enabling scheduled CI.

The `--upstream-repo` and `--upstream-branch` CLI flags are for a local experiment only. CI must
use the checked-in configuration so an unreviewed workflow input cannot redirect synchronization.

## Reusable workflow trial

Pin the reusable workflow and the installed binary to the same immutable commit:

```yaml
jobs:
  pull:
    uses: Natural-selection1/josh-sync/.github/workflows/bluekernel-pull.yml@<josh-sync-commit>
    with:
      github-app-id: ${{ vars.APP_CLIENT_ID }}
      pr-author: "<your-app>[bot]"
      josh-sync-repository: Natural-selection1/josh-sync
      josh-sync-revision: <josh-sync-commit>
    secrets:
      github-app-secret: ${{ secrets.APP_PRIVATE_KEY }}
```

Enable the reusable push workflow for one subtree at a time. The first production pilot is
`kernel`; verify no-op handling, force-with-lease recovery, and exact PR reuse before enabling the
same caller workflow in the remaining component repositories.
