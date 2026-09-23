# BlueOS Josh sync utilities
This repository contains a binary utility for performing [Josh](https://github.com/josh-project/josh)
synchronizations (pull and push) between a subtree repository and the BlueOS monorepo configured in that subtree.

## Installation
Install a specific commit of the binary so that it matches the reusable workflow revision:

```bash
$ cargo install --locked \
  --git https://github.com/<josh-sync-owner>/josh-sync \
  --rev <josh-sync-commit>
```

## Creating config file

First, create a configuration file for a given subtree repo using `vivoblueos-josh-sync init`. The config will be created under the path `josh-sync.toml`. It is tracked in the subtree repository, so the monorepo source is reviewed together with the mapping.

For a Natural-selection1 trial, a kernel mapping looks like this:

```toml
org = "Natural-selection1"
repo = "kernel"
upstream-repo = "Natural-selection1/blueos-mono"
upstream-branch = "main"
path = "kernel"
filter-version = 2
```

For production, change the reviewed `upstream-repo` to `vivoblueos/blueos` (and `org` to `vivoblueos` if the subtree also moves). Do not pass a different upstream from CI.

If you need to specify a more complex Josh `filter`, use `filter` field in the configuration file instead of the `path` field.

The `init` command will also create an empty `blueos-version` file (if it doesn't already exist) that stores the last configured monorepo SHA that was synced in the subtree.

### Repository mapping examples

Repositories that map directly to a top-level path use the repository name as `path`:

```toml
repo = "kernel"
path = "kernel"
```

Repositories nested under `apps/` use a nested path. In this example, `apps_shell` maps to `apps/shell` in the configured monorepo:

```toml
repo = "apps_shell"
path = "apps/shell"
```

`upstream-branch` selects the monorepo branch. A subtree with a default branch other than `main` does not need a special `josh-sync.toml` setting; configure its PR base only in the CI workflow that consumes this tool:

```yaml
pr-base-branch: blueos-dev
```

The [`josh-sync.example.toml`](josh-sync.example.toml) file contains all the things that can be configured.

## Performing pull

A pull operation resolves the configured `upstream-branch`, fetches its subtree projection, and merges it into the subtree repository. After performing a pull, a pull request is sent against the *subtree repository*.

1) Checkout the latest default branch of the subtree
2) Create a new branch that will be used for the subtree PR, e.g. `pull`
3) Run `vivoblueos-josh-sync pull`
4) Send a PR to the subtree repository

- Note that `vivoblueos-josh-sync` can do this for you if you have the [gh](https://cli.github.com/) CLI tool installed.

You can also configure a set of postprocessing operations to be performed after a successful pull using the `post-pull` configuration.

## Performing push

A push operation takes changes performed in the subtree repository and merges them into the subtree subdirectory of the configured BlueOS monorepo. After performing a push, a PR is sent against that monorepo's configured `upstream-branch`.

1) Checkout the latest default branch of the subtree
2) Run `vivoblueos-josh-sync push <branch> <your-github-username>`

- The branch with the push contents will be created in the `<your-github-username>/<configured-monorepo-name>` fork, in the `<branch>` branch.

3) Send a PR to the configured BlueOS monorepo.

## Automating pulls on CI

This repository contains a reusable workflow for performing the `pull` operation from CI. The workflow does the following:

1) Installs a pinned `vivoblueos-josh-sync` revision (which manages Josh)
2) Performs a `pull` operation
3) Either creates a new PR (if it did not exist) with the resulting pull branch or force-pushes to an existing PR on the subtree repository

Use [`blueos-pull.example.yml`](blueos-pull.example.yml) as the starting point for a subtree
repository. The example pins the reusable workflow, its scripts, and the installed binary to the
same immutable commit; keep those SHA values in sync when updating the revision.

You will need to have a GitHub App configured on the repository with write permissions for
contents and pull requests. Synchronization PRs are labeled `josh-sync` by default; use
the optional `pr-label` input to choose another label.

Both reusable workflows require an `approval-user-token` secret from a distinct GitHub user
with Write access to the target repository and a fine-grained token with Pull requests: write.
The App creates or updates the PR, the user token approves its current head, and the App
enables auto-merge. The workflow refuses to approve a PR with an unexpected author, head,
base, or repository. A fine-grained token is limited to one resource owner, so lab and
production organizations need separate tokens.

Both synchronization workflows read the organization or repository Actions variable
`JOSH_SYNC_AUTO_MERGE`. Set it to `true` to enable GitHub auto-merge for generated
synchronization PRs in both directions; leave it unset or set it to `false` to keep the
PR open for manual merging. Auto-merge uses a merge commit and waits for the target
repository's configured requirements. A conflicting PR is left open and reported by the
workflow.

## Automating pushes on CI

The reusable `blueos-push.yml` workflow pushes the caller's default-branch state into a stable,
CI-owned branch in the configured BlueOS monorepo. It creates a monorepo pull request or updates
the existing pull request for that exact head and base branch. If the full filtered trees already
match, including `blueos-version`, the workflow succeeds without changing the branch or PR.

Use [`blueos-push.example.yml`](blueos-push.example.yml) as the starting point for a subtree
repository. The example pins the reusable workflow, its scripts, and the installed binary to the
same immutable commit; keep those SHA values in sync when updating the revision.

The GitHub App must be installed on the configured monorepo with repository contents and pull
request write permissions. The generated PR is labeled `josh-sync` by default. The
default sync branch is
`github.com/<subrepo-owner>/<subrepo>/josh-sync`. Only this workflow may update that branch.

The generated monorepo pull request must be merged with a merge commit. Do not amend, squash, or
rebase commits produced by the sync tool.

The reusable workflows invoke the synchronizer with `--no-interact`. This flag suppresses
josh-sync confirmation prompts and uses each prompt's safe default: a missing local monorepo
checkout is cloned, while the optional local `gh` pull-request prompt is declined. The push step
also disables Git credential prompts so missing credentials fail immediately instead of blocking CI.

See [test.md](test.md) for the Natural-selection1 trial sequence and the production migration boundary.

## Git peculiarities

NOTE: If you use Git/SSH protocol to push to your fork of the configured monorepo,
ensure that you have this entry in your Git config,
else the 2 steps that follow would prompt for a username and password:

```
[url "git@github.com:"]
insteadOf = "https://github.com/"
```

### Minimal git config

For simplicity (ease of implementation purposes), the josh-sync script simply calls out to system git. This means that the git invocation may be influenced by global (or local) git configuration.

You may observe "Nothing to pull" even if you *know* blueos-pull has something to pull if your global git config sets `fetch.prunetags = true` (and possibly other configurations may cause unexpected outcomes).

To minimize the likelihood of this happening, you may wish to keep a separate *minimal* git config that *only* has `[user]` entries from global git config, then repoint system git to use the minimal git config instead. E.g.

```
GIT_CONFIG_GLOBAL=/path/to/minimal/gitconfig GIT_CONFIG_SYSTEM='' vivoblueos-josh-sync ...
```
