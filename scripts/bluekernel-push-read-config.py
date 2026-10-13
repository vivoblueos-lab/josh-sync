import os
import re
import subprocess

import tomllib

github_name = re.compile(r"[A-Za-z0-9_.-]+")


def github_repository(value, label):
    if not isinstance(value, str):
        raise SystemExit(f"{label} must be a string")
    parts = value.split("/")
    if len(parts) != 2 or not all(github_name.fullmatch(part) for part in parts):
        raise SystemExit(f"invalid {label}: {value!r}")
    return parts


def git_branch(value, label):
    if not isinstance(value, str) or not value:
        raise SystemExit(f"{label} must not be empty")
    result = subprocess.run(
        ["git", "check-ref-format", "--branch", value],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"invalid {label}: {value!r}")


with open("josh-sync.toml", "rb") as config_file:
    config = tomllib.load(config_file)

upstream = config.get("upstream-repo", "")
parts = github_repository(upstream, "upstream-repo")
upstream_branch = config.get("upstream-branch", "")
git_branch(upstream_branch, "upstream-branch")
subrepo = config.get("repo", "")
if not isinstance(subrepo, str) or not github_name.fullmatch(subrepo):
    raise SystemExit(f"invalid repo: {subrepo!r}")

github_repository(os.environ["JOSH_SYNC_REPOSITORY"], "josh-sync-repository")
revision = os.environ["JOSH_SYNC_REVISION"]
if not re.fullmatch(r"[0-9a-f]{40}", revision):
    raise SystemExit("josh-sync-revision must be a full lowercase commit SHA")

branch = os.environ["REQUESTED_BRANCH"]
if not branch:
    branch = f"github.com/{os.environ['GITHUB_REPOSITORY']}/josh-sync"
git_branch(branch, "branch-name")

with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
    output.write(f"upstream={upstream}\n")
    output.write(f"upstream-owner={parts[0]}\n")
    output.write(f"upstream-repository={parts[1]}\n")
    output.write(f"upstream-branch={upstream_branch}\n")
    output.write(f"subrepo={subrepo}\n")
    output.write(f"branch={branch}\n")
