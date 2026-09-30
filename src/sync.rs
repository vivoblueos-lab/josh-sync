use crate::SyncContext;
use crate::config::{JoshConfig, PostPullOperation};
use crate::josh::{JoshFilter, JoshProxy, try_install_josh_filter};
use crate::utils::{ensure_clean_git_state, prompt};
use crate::utils::{get_current_head_sha, run_command_at};
use crate::utils::{run_command, stream_command};
use anyhow::{Context, Error};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const NO_REBASE_WARN: &str = "Do NOT amend/squash/rebase any of the commits produced by this tool; that can badly break future syncs.";

pub enum BlueosPullError {
    /// No changes are available to be pulled.
    NothingToPull,
    /// A BlueOS pull has failed, probably a git operation error has occurred.
    PullFailed(anyhow::Error),
}

impl From<anyhow::Error> for BlueosPullError {
    fn from(error: Error) -> Self {
        Self::PullFailed(error)
    }
}

pub enum BlueosPushError {
    /// The subtree projection already matches the configured monorepo branch.
    NothingToPush,
    /// A reverse synchronization operation failed.
    PushFailed(anyhow::Error),
}

impl From<anyhow::Error> for BlueosPushError {
    fn from(error: Error) -> Self {
        Self::PushFailed(error)
    }
}

#[derive(Copy, Clone)]
pub enum FilterVersion {
    /// Keep empty merge commits.
    Version1,
    /// Skip empty merge commits.
    Version2,
}

impl FilterVersion {
    pub fn latest() -> Self {
        Self::Version2
    }
}

pub struct PullResult {
    pub merge_commit_message: String,
}

pub struct GitSync {
    context: SyncContext,
    proxy: JoshProxy,
    verbose: bool,
}

impl GitSync {
    pub fn new(context: SyncContext, proxy: JoshProxy, verbose: bool) -> Self {
        Self {
            context,
            proxy,
            verbose,
        }
    }

    pub fn blueos_pull(
        &self,
        upstream_repo: String,
        upstream_branch: String,
        upstream_commit: Option<String>,
        allow_noop: bool,
    ) -> Result<PullResult, BlueosPullError> {
        // The upstream commit that we want to pull
        let upstream_sha = if let Some(sha) = upstream_commit {
            sha
        } else {
            let out = run_command(
                [
                    "git",
                    "ls-remote",
                    &format!("https://github.com/{upstream_repo}"),
                    &format!("refs/heads/{upstream_branch}"),
                ],
                self.verbose,
            )
            .context("cannot fetch upstream commit")?;
            out.split_whitespace()
                .next()
                .unwrap_or_else(|| {
                    panic!(
                        "Could not obtain BlueOS monorepo branch `{upstream_branch}` from remote: '{out}'"
                    )
                })
                .to_owned()
        };

        ensure_clean_git_state(self.verbose)?;

        // Make sure josh is running.
        let josh = self
            .proxy
            .start(&self.context.config, false)
            .context("cannot start josh-proxy")?;
        let josh_url = josh.git_url(
            &upstream_repo,
            Some(&upstream_sha),
            &construct_josh_filter(&self.context.config),
        );

        let orig_head = get_current_head_sha(self.verbose)?;
        println!(
            "previous upstream base: {}",
            self.context
                .last_upstream_sha
                .as_deref()
                .unwrap_or("<none>"),
        );
        println!("new upstream base: {upstream_sha}");
        println!("original local HEAD: {orig_head}");

        // If the upstream SHA hasn't changed from the latest sync, there is nothing to pull
        // We distinguish this situation for tools that might not want to consider this to
        // be an error.
        if let Some(previous_base_commit) = self.context.last_upstream_sha.as_ref()
            && *previous_base_commit == upstream_sha
        {
            return Err(BlueosPullError::NothingToPull);
        }

        // Create a checkpoint to which we reset if something unusual happens
        let mut git_reset = GitResetOnDrop::new(orig_head, self.verbose);

        // Update the last upstream SHA file. As a separate commit, since making it part of
        // the merge has confused the heck out of josh in the past.
        // We pass `--no-verify` to avoid running git hooks.
        // We do this before the merge so that if there are merge conflicts, we have
        // the right blueos-version file while resolving them.
        std::fs::write(
            &self.context.last_upstream_sha_path,
            format!("{upstream_sha}\n"),
        )
        .with_context(|| {
            anyhow::anyhow!(
                "cannot write upstream SHA to {}",
                self.context.last_upstream_sha_path.display()
            )
        })?;

        let prep_message = format!(
            r#"Prepare for merging from {upstream_repo}

This updates the blueos-version file to {upstream_sha}."#,
        );

        let blueos_version_path = self
            .context
            .last_upstream_sha_path
            .to_string_lossy()
            .to_string();
        // Add the file to git index, in case this is the first time we perform the sync
        // Otherwise `git commit <file>` below wouldn't work.
        run_command(["git", "add", &blueos_version_path], self.verbose)?;
        run_command(
            [
                "git",
                "commit",
                &blueos_version_path,
                "--no-verify",
                "-m",
                &prep_message,
            ],
            self.verbose,
        )
        .context("cannot create preparation commit")?;

        // Fetch the given BlueOS monorepo commit.
        run_command(["git", "fetch", &josh_url], self.verbose)
            .context("cannot fetch git state through Josh")?;

        // This should not add any new root commits. So count those before and after merging.
        let num_roots = || -> anyhow::Result<u32> {
            Ok(run_command(
                ["git", "rev-list", "HEAD", "--max-parents=0", "--count"],
                self.verbose,
            )
            .context("failed to determine the number of root commits")?
            .parse::<u32>()?)
        };
        let num_roots_before = num_roots()?;

        let sha_pre_merge = get_current_head_sha(self.verbose)?;

        // The filtered SHA of upstream
        let incoming_ref = run_command(["git", "rev-parse", "FETCH_HEAD"], self.verbose)?;
        println!("incoming ref: {incoming_ref}");

        let merge_message = format!(
            r#"Merge ref '{upstream_head_short}' from {upstream_repo}

Pull recent changes from https://github.com/{upstream_repo} via Josh.

Upstream ref: {upstream_repo}@{upstream_sha}
Filtered ref: {sub_org}/{sub_repo}@{incoming_ref}
Upstream diff: https://github.com/{upstream_repo}/compare/{prev_upstream_sha}...{upstream_sha}

This merge was created using vivoblueos-josh-sync.
"#,
            upstream_head_short = &upstream_sha[..12],
            sub_org = self.context.config.org,
            sub_repo = self.context.config.repo,
            prev_upstream_sha = self
                .context
                .last_upstream_sha
                .as_deref()
                .unwrap_or(&upstream_sha)
        );

        // Merge the fetched commit.
        // It is useful to print stdout/stderr here, because it shows the git diff summary
        if let Err(error) = stream_command(
            [
                "git",
                "merge",
                "FETCH_HEAD",
                "--no-verify",
                "--no-ff",
                "-m",
                &merge_message,
            ],
            self.verbose,
        )
        .context("FAILED to merge new commits, something went wrong")
        {
            eprintln!(
                r"The merge was unsuccessful (maybe there was a conflict?).
NOT rolling back the branch state, so you can examine it manually.
After you fix the conflicts, `git add` the changes and run `git merge --continue`."
            );
            eprintln!("{NO_REBASE_WARN}");
            git_reset.disarm();
            return Err(BlueosPullError::PullFailed(error));
        }

        // Now detect if something has actually been pulled
        let current_sha = get_current_head_sha(self.verbose)?;

        // This is the easy case, no merge was performed, so we bail, unless `allow_noop` is true
        if current_sha == sha_pre_merge && !allow_noop {
            eprintln!("No merge was performed, no changes to pull were found. Rolling back.");
            return Err(BlueosPullError::NothingToPull);
        }

        // But it can be more tricky - we can have only empty merge/rollup merge commits from
        // the BlueOS monorepo, so a merge was created, but the in-tree diff can still be empty.
        // In that case we also bail, unless `allow_noop` is true.
        if self.has_empty_diff(&sha_pre_merge) && !allow_noop {
            eprintln!("Only empty changes were pulled. Rolling back.");
            return Err(BlueosPullError::NothingToPull);
        }

        println!("Pull finished! Current HEAD is {current_sha}");
        println!("{NO_REBASE_WARN}");

        if !self.context.config.post_pull.is_empty() {
            println!("Running post-pull operation(s)");

            for op in &self.context.config.post_pull {
                self.run_post_pull_op(op)?;
            }
        }

        git_reset.disarm();

        // Check that the number of roots did not change.
        if num_roots()? != num_roots_before {
            return Err(anyhow::anyhow!(
                "Josh created a new root commit. This is probably not the history you want."
            )
            .into());
        }

        Ok(PullResult {
            merge_commit_message: merge_message,
        })
    }

    pub fn blueos_push(
        &self,
        username: &str,
        branch: &str,
        update_existing: bool,
        no_interact: bool,
    ) -> Result<(), BlueosPushError> {
        ensure_clean_git_state(self.verbose)?;

        let base_upstream_sha = self
            .context
            .last_upstream_sha
            .clone()
            .filter(|sha| !sha.is_empty())
            .ok_or_else(|| anyhow::anyhow!("blueos-version does not contain an upstream SHA"))?;
        let upstream_repo = &self.context.config.upstream_repo;
        let fork_repo = format!("{username}/{}", self.context.config.upstream_repo_name()?);

        // Pushes to public GitHub repositories still need the downstream client to provide
        // credentials, so require authentication on the local proxy.
        let josh = self
            .proxy
            .start(&self.context.config, true)
            .context("cannot start josh-proxy")?;
        let filter = construct_josh_filter(&self.context.config);
        let josh_url = josh.git_url(&fork_repo, None, &filter);
        let user_upstream_url = format!("https://github.com/{fork_repo}");
        let current_dir = std::env::current_dir().context("cannot determine current directory")?;
        let existing_branch =
            resolve_remote_branch(&user_upstream_url, branch, &current_dir, self.verbose)?;
        ensure_branch_update_allowed(
            existing_branch.as_deref(),
            update_existing,
            branch,
            &user_upstream_url,
        )?;

        let upstream_sha = resolve_remote_branch(
            &format!("https://github.com/{upstream_repo}"),
            &self.context.config.upstream_branch,
            &current_dir,
            self.verbose,
        )?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "configured upstream branch '{}/{}' does not exist",
                upstream_repo,
                self.context.config.upstream_branch
            )
        })?;
        let upstream_josh_url = josh.git_url(upstream_repo, Some(&upstream_sha), &filter);
        run_command(["git", "fetch", &upstream_josh_url], self.verbose)
            .context("cannot fetch the current upstream subtree through Josh")?;
        let local_head = self.local_roundtrip_head(&self.context.config)?;
        let upstream_head = run_command(["git", "rev-parse", "FETCH_HEAD"], self.verbose)
            .context("failed to resolve the current upstream subtree")?;
        ensure_push_needed(&local_head, &upstream_head, &current_dir, self.verbose)?;

        let blueos_git = prepare_blueos_checkout(upstream_repo, no_interact, self.verbose)
            .context("cannot prepare BlueOS monorepo checkout")?;

        // Convert against the last pulled base without exposing that intermediate
        // state on the PR branch. GitHub can close the PR when its head is reset
        // to a commit already reachable from the base branch.
        println!("Preparing {user_upstream_url} (base: {base_upstream_sha})...");

        // Download the base upstream SHA
        run_command_at(
            [
                "git",
                "fetch",
                &format!("https://github.com/{upstream_repo}"),
                &base_upstream_sha,
            ],
            &blueos_git,
            self.verbose,
        )
        .context("cannot download latest upstream SHA")?;

        publish_staged_update(
            &blueos_git,
            &user_upstream_url,
            branch,
            &base_upstream_sha,
            existing_branch.as_deref(),
            self.verbose,
            |stage_branch| {
                println!("Converting changes on {stage_branch}...");
                run_command(
                    ["git", "push", &josh_url, &format!("HEAD:{stage_branch}")],
                    self.verbose,
                )?;
                run_command_at(
                    ["git", "fetch", &user_upstream_url, stage_branch],
                    &blueos_git,
                    self.verbose,
                )?;
                let candidate = run_command_at(
                    ["git", "rev-parse", "FETCH_HEAD"],
                    &blueos_git,
                    self.verbose,
                )?;
                let candidate_url = josh.git_url(&fork_repo, Some(&candidate), &filter);
                self.roundtrip_check(&self.context.config, &candidate_url)?;
                Ok(candidate)
            },
        )
        .context("cannot publish the verified sync branch")?;
        println!("Published verified changes to {branch}. Please create a BlueOS monorepo PR.");
        println!("{NO_REBASE_WARN}");

        Ok(())
    }

    fn local_roundtrip_head(&self, config: &JoshConfig) -> anyhow::Result<String> {
        if let Some(subtree_filter) = &config.subtree_filter {
            let josh_filter = get_josh_filter(self.verbose)?;
            josh_filter.run(
                [subtree_filter, "HEAD"],
                &std::env::current_dir()?,
                self.verbose,
            )?;
            run_command(["git", "rev-parse", "FILTERED_HEAD"], self.verbose)
                .context("failed to get FILTERED_HEAD")
        } else {
            get_current_head_sha(self.verbose)
        }
    }

    fn has_empty_diff(&self, baseline_sha: &str) -> bool {
        // `git diff --exit-code` "succeeds" if the diff is empty.
        run_command(["git", "diff", "--exit-code", baseline_sha], self.verbose).is_ok()
    }

    fn run_post_pull_op(&self, op: &PostPullOperation) -> anyhow::Result<()> {
        let head = get_current_head_sha(self.verbose)?;
        run_command(op.cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>(), true)?;
        if !self.has_empty_diff(&head) {
            println!(
                "`{}` changed something, committing with message `{}`",
                op.cmd.join(" "),
                op.commit_message
            );
            run_command(["git", "add", "-u"], self.verbose)?;
            run_command(["git", "commit", "-m", &op.commit_message], self.verbose)?;
        }

        Ok(())
    }

    fn roundtrip_check(&self, config: &JoshConfig, josh_url: &str) -> anyhow::Result<()> {
        run_command_at(
            ["git", "fetch", josh_url],
            &std::env::current_dir()?,
            self.verbose,
        )?;
        let head = self.local_roundtrip_head(config)?;
        let fetch_head = run_command(["git", "rev-parse", "FETCH_HEAD"], self.verbose)?;
        validate_roundtrip(&head, &fetch_head)?;
        println!(
            "Confirmed that the candidate round-trips back to {} properly.",
            self.context.config.repo
        );
        Ok(())
    }
}

fn resolve_remote_branch(
    remote: &str,
    branch: &str,
    workdir: &Path,
    verbose: bool,
) -> anyhow::Result<Option<String>> {
    let remote_ref = format!("refs/heads/{branch}");
    let mut command = Command::new("git");
    command
        .current_dir(workdir)
        .args(["ls-remote", "--exit-code", remote, &remote_ref]);
    if verbose {
        eprintln!("+ {command:?}");
    }
    let output = command.output().context("unable to run git ls-remote")?;
    parse_remote_branch_output(
        output.status.code(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        remote,
        branch,
    )
}

fn parse_remote_branch_output(
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    remote: &str,
    branch: &str,
) -> anyhow::Result<Option<String>> {
    match exit_code {
        Some(0) => {
            let sha = stdout
                .split_whitespace()
                .next()
                .ok_or_else(|| anyhow::anyhow!("git ls-remote returned no branch SHA"))?;
            Ok(Some(sha.to_owned()))
        }
        Some(2) => Ok(None),
        _ => {
            let stderr = stderr.trim();
            Err(anyhow::anyhow!(
                "git ls-remote failed for {remote} branch {branch}: {stderr}"
            ))
        }
    }
}

fn ensure_branch_update_allowed(
    existing_sha: Option<&str>,
    update_existing: bool,
    branch: &str,
    remote: &str,
) -> anyhow::Result<()> {
    if existing_sha.is_some() && !update_existing {
        return Err(anyhow::anyhow!(
            "The branch '{branch}' seems to already exist in '{remote}'. Please delete it and try again."
        ));
    }
    Ok(())
}

fn push_commit_to_branch(
    workdir: &Path,
    remote: &str,
    branch: &str,
    commit: &str,
    existing_sha: Option<&str>,
    verbose: bool,
) -> anyhow::Result<()> {
    let refspec = format!("{commit}:refs/heads/{branch}");
    // An empty expected SHA requires the branch to remain absent, including
    // when another caller creates an otherwise fast-forwardable branch.
    let lease = format!(
        "--force-with-lease=refs/heads/{branch}:{}",
        existing_sha.unwrap_or_default()
    );
    run_command_at(["git", "push", &lease, remote, &refspec], workdir, verbose)?;
    Ok(())
}

fn publish_staged_update(
    workdir: &Path,
    remote: &str,
    branch: &str,
    base: &str,
    existing_sha: Option<&str>,
    verbose: bool,
    prepare_and_validate: impl FnOnce(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    let stage = RemoteStageBranch::create(workdir, remote, base, verbose)?;
    let candidate = prepare_and_validate(&stage.branch)?;
    push_commit_to_branch(workdir, remote, branch, &candidate, existing_sha, verbose)
        .context("sync branch changed during conversion, or publication failed")
    // The staging branch is cleaned up on both success and error. Cleanup
    // failure is a warning and never replaces the operation's result.
}

struct RemoteStageBranch {
    workdir: PathBuf,
    remote: String,
    branch: String,
    verbose: bool,
    _name_reservation: tempfile::TempDir,
}

impl RemoteStageBranch {
    fn create(workdir: &Path, remote: &str, base: &str, verbose: bool) -> anyhow::Result<Self> {
        let reservation = tempfile::Builder::new()
            .prefix("blueos-josh-stage-")
            .rand_bytes(16)
            .tempdir()?;
        let branch = reservation
            .path()
            .file_name()
            .context("temporary branch name is missing")?
            .to_string_lossy()
            .into_owned();
        println!("Preparing temporary branch {branch}...");
        push_commit_to_branch(workdir, remote, &branch, base, None, verbose)
            .with_context(|| {
                format!("cannot create temporary branch {branch}; check this ref if the remote response was interrupted")
            })?;
        Ok(Self {
            workdir: workdir.to_owned(),
            remote: remote.to_owned(),
            branch,
            verbose,
            _name_reservation: reservation,
        })
    }

    fn cleanup(&self) -> anyhow::Result<()> {
        if let Some(sha) =
            resolve_remote_branch(&self.remote, &self.branch, &self.workdir, self.verbose)?
        {
            let lease = format!("--force-with-lease=refs/heads/{}:{sha}", self.branch);
            let refspec = format!(":refs/heads/{}", self.branch);
            run_command_at(
                ["git", "push", &lease, &self.remote, &refspec],
                &self.workdir,
                self.verbose,
            )?;
        }
        Ok(())
    }
}

impl Drop for RemoteStageBranch {
    fn drop(&mut self) {
        match self.cleanup() {
            Ok(()) => println!("Cleaned up temporary branch {}.", self.branch),
            Err(error) => eprintln!(
                "Warning: cannot clean up temporary branch {} on {}: {error:#}",
                self.branch, self.remote
            ),
        }
    }
}

fn refs_have_same_tree(
    left: &str,
    right: &str,
    workdir: &Path,
    verbose: bool,
) -> anyhow::Result<bool> {
    let left_tree = run_command_at(
        ["git", "rev-parse", &format!("{left}^{{tree}}")],
        workdir,
        verbose,
    )
    .context("failed to resolve local subtree tree")?;
    let right_tree = run_command_at(
        ["git", "rev-parse", &format!("{right}^{{tree}}")],
        workdir,
        verbose,
    )
    .context("failed to resolve upstream subtree tree")?;
    Ok(left_tree == right_tree)
}

fn ensure_push_needed(
    local_head: &str,
    upstream_head: &str,
    workdir: &Path,
    verbose: bool,
) -> Result<(), BlueosPushError> {
    if refs_have_same_tree(local_head, upstream_head, workdir, verbose)? {
        return Err(BlueosPushError::NothingToPush);
    }
    Ok(())
}

fn validate_roundtrip(expected: &str, actual: &str) -> anyhow::Result<()> {
    if expected != actual {
        return Err(anyhow::anyhow!(
            "Josh created a non-roundtrip push! Do NOT merge this into the BlueOS monorepo!\n\
            Expected {expected}, got {actual}."
        ));
    }
    Ok(())
}

// This is called only when the `subtree-filter` is set.
fn get_josh_filter(verbose: bool) -> anyhow::Result<JoshFilter> {
    println!("Updating/installing josh-filter binary...");
    match try_install_josh_filter(verbose) {
        Some(filter) => Ok(filter),
        None => Err(anyhow::anyhow!("Could not install josh-filter")),
    }
}

/// Find a BlueOS monorepo we can do our push preparation in.
fn prepare_blueos_checkout(
    upstream_repo: &str,
    no_interact: bool,
    verbose: bool,
) -> anyhow::Result<PathBuf> {
    if let Ok(blueos_git) = std::env::var("BLUEOS_GIT") {
        let blueos_git = PathBuf::from(blueos_git);
        assert!(
            blueos_git.is_dir(),
            "BlueOS monorepo checkout path must be a directory"
        );
        return Ok(blueos_git);
    };

    // Otherwise, download it
    let path = "blueos-checkout";
    if !Path::new(path).join(".git").exists() {
        if prompt(
            &format!(
                "Path to a BlueOS monorepo checkout is not configured via the BLUEOS_GIT environment variable, and {path} directory was not found. Do you want to download a BlueOS checkout into {path}?",
            ),
            // Automatically clone when interaction is disabled or on CI.
            true,
            no_interact,
        ) {
            println!(
                "Cloning the BlueOS monorepo into `{path}`. Use the BLUEOS_GIT environment variable to override the location of the checkout"
            );
            // Stream stdout/stderr to the terminal, so that the user sees clone progress
            stream_command(
                [
                    "git",
                    "clone",
                    "--filter=blob:none",
                    &format!("https://github.com/{upstream_repo}"),
                    path,
                ],
                verbose,
            )
            .context("cannot clone the BlueOS monorepo")?;
        } else {
            return Err(anyhow::anyhow!(
                "cannot continue without a BlueOS monorepo checkout"
            ));
        }
    }
    Ok(PathBuf::from(path))
}

/// Restores HEAD to `reset_to` on drop, unless `disarm` is called first.
struct GitResetOnDrop {
    disarmed: bool,
    reset_to: String,
    verbose: bool,
}

impl GitResetOnDrop {
    fn new(current_sha: String, verbose: bool) -> Self {
        Self {
            disarmed: false,
            reset_to: current_sha,
            verbose,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for GitResetOnDrop {
    fn drop(&mut self) {
        if !self.disarmed {
            eprintln!("Reverting HEAD to {}", self.reset_to);
            run_command(["git", "reset", "--hard", &self.reset_to], self.verbose)
                .unwrap_or_else(|_| panic!("cannot reset current branch to {}", self.reset_to));
        }
    }
}

fn construct_josh_filter(config: &JoshConfig) -> String {
    let filter = match (&config.path, &config.filter) {
        (Some(path), None) => format!(":/{path}"),
        (None, Some(filter)) => filter.clone(),
        _ => panic!("Config contains both path and a filter"),
    };
    match config.filter_version {
        // Keep backwards compatibility with repositories that started with a legacy version of
        // Josh.
        FilterVersion::Version1 => {
            // Convert old :rev syntax
            let filter = convert_rev_syntax(&filter);
            // Keep empty merges
            wrap_compat(&filter)
        }
        // Use the current default behavior of Josh.
        FilterVersion::Version2 => filter,
    }
}

/// Converts filters from old `:rev(sha:filter)` syntax to new
/// `:rev(<=sha:filter)` syntax. Null SHAs (40 zeros) become `_`.
/// Only touches SHAs inside `:rev(...)` blocks.
fn convert_rev_syntax(input: &str) -> String {
    let rev_block = regex::Regex::new(r":rev\([^)]*\)").unwrap();
    let entry = regex::Regex::new(
        r"(?x)
        ([,(])                # delimiter before entry
        (0{40}|[0-9a-f]{40})  # full SHA
        :                     # colon separator
    ",
    )
    .unwrap();

    rev_block
        .replace_all(input, |block: &regex::Captures| {
            entry
                .replace_all(&block[0], |caps: &regex::Captures| {
                    let delim = &caps[1];
                    let sha = &caps[2];
                    if sha.chars().all(|c| c == '0') {
                        format!("{delim}_:")
                    } else {
                        format!("{delim}<={sha}:")
                    }
                })
                .into_owned()
        })
        .into_owned()
}

/// Wraps a filter with the backwards compatibility meta options for
/// trivial merge preservation and CRLF normalization in gpgsig headers.
///
/// `:your/filter` becomes
/// `:~(history="keep-trivial-merges",gpgsig="norm-lf")[:your/filter]`
fn wrap_compat(filter: &str) -> String {
    format!(":~(history=\"keep-trivial-merges\",gpgsig=\"norm-lf\")[{filter}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn git(workdir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(workdir)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn test_repository() -> (TempDir, PathBuf, PathBuf) {
        let temp = TempDir::new().unwrap();
        let worktree = temp.path().join("worktree");
        let remote = temp.path().join("remote.git");
        fs::create_dir(&worktree).unwrap();
        git(&worktree, &["init", "-q"]);
        git(&worktree, &["config", "user.name", "CI"]);
        git(&worktree, &["config", "user.email", "ci@example.com"]);
        git(
            temp.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );
        (temp, worktree, remote)
    }

    fn commit_file(worktree: &Path, path: &str, contents: &str, message: &str) -> String {
        fs::write(worktree.join(path), contents).unwrap();
        git(worktree, &["add", path]);
        git(worktree, &["commit", "-qm", message]);
        git(worktree, &["rev-parse", "HEAD"])
    }

    #[test]
    fn resolves_existing_and_missing_remote_branches() {
        let (_temp, worktree, remote) = test_repository();
        let head = commit_file(&worktree, "file", "one\n", "one");
        git(
            &worktree,
            &["push", remote.to_str().unwrap(), "HEAD:refs/heads/sync"],
        );

        assert_eq!(
            resolve_remote_branch(remote.to_str().unwrap(), "sync", &worktree, false).unwrap(),
            Some(head)
        );
        assert_eq!(
            resolve_remote_branch(remote.to_str().unwrap(), "missing", &worktree, false).unwrap(),
            None
        );
        assert!(resolve_remote_branch("/does/not/exist", "sync", &worktree, false).is_err());
    }

    #[test]
    fn operational_remote_errors_are_not_missing_branches() {
        let authentication_error = parse_remote_branch_output(
            Some(128),
            "",
            "fatal: Authentication failed",
            "https://example.invalid/repo",
            "sync",
        )
        .unwrap_err()
        .to_string();
        assert!(authentication_error.contains("Authentication failed"));

        let network_error = parse_remote_branch_output(
            Some(128),
            "",
            "fatal: Could not resolve host: example.invalid",
            "https://example.invalid/repo",
            "sync",
        )
        .unwrap_err()
        .to_string();
        assert!(network_error.contains("Could not resolve host"));
    }

    #[test]
    fn existing_branch_requires_explicit_update() {
        assert!(
            ensure_branch_update_allowed(Some("0123456789abcdef"), false, "sync", "remote")
                .is_err()
        );
        ensure_branch_update_allowed(Some("0123456789abcdef"), true, "sync", "remote").unwrap();
        ensure_branch_update_allowed(None, false, "sync", "remote").unwrap();
    }

    #[test]
    fn creates_and_updates_remote_branch_with_a_lease() {
        let (_temp, worktree, remote) = test_repository();
        let first = commit_file(&worktree, "file", "one\n", "one");
        let second = commit_file(&worktree, "file", "two\n", "two");

        push_commit_to_branch(
            &worktree,
            remote.to_str().unwrap(),
            "sync",
            &first,
            None,
            false,
        )
        .unwrap();
        push_commit_to_branch(
            &worktree,
            remote.to_str().unwrap(),
            "sync",
            &second,
            Some(&first),
            false,
        )
        .unwrap();

        assert_eq!(
            resolve_remote_branch(remote.to_str().unwrap(), "sync", &worktree, false).unwrap(),
            Some(second)
        );
    }

    #[test]
    fn rejects_a_stale_branch_lease() {
        let (_temp, worktree, remote) = test_repository();
        let first = commit_file(&worktree, "file", "one\n", "one");
        let second = commit_file(&worktree, "file", "two\n", "two");
        let third = commit_file(&worktree, "file", "three\n", "three");
        push_commit_to_branch(
            &worktree,
            remote.to_str().unwrap(),
            "sync",
            &first,
            None,
            false,
        )
        .unwrap();
        git(
            &worktree,
            &[
                "push",
                "--force",
                remote.to_str().unwrap(),
                &format!("{second}:refs/heads/sync"),
            ],
        );

        assert!(
            push_commit_to_branch(
                &worktree,
                remote.to_str().unwrap(),
                "sync",
                &third,
                Some(&first),
                false,
            )
            .is_err()
        );
    }

    fn staging_refs(remote: &Path) -> String {
        git(
            remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/blueos-josh-stage-*",
            ],
        )
    }

    #[test]
    fn publishes_only_the_verified_candidate_and_cleans_up() {
        let (_temp, worktree, remote) = test_repository();
        let base = commit_file(&worktree, "file", "base\n", "base");
        let previous = commit_file(&worktree, "file", "previous\n", "previous");
        let candidate = commit_file(&worktree, "file", "candidate\n", "candidate");
        let remote_url = remote.to_str().unwrap();
        for expected in [None, Some(previous.as_str())] {
            if let Some(sha) = expected {
                push_commit_to_branch(&worktree, remote_url, "sync", sha, Some(&candidate), false)
                    .unwrap();
            }
            publish_staged_update(
                &worktree,
                remote_url,
                "sync",
                &base,
                expected,
                false,
                |stage| {
                    assert_eq!(
                        resolve_remote_branch(remote_url, "sync", &worktree, false)?,
                        expected.map(str::to_owned)
                    );
                    assert_eq!(
                        resolve_remote_branch(remote_url, stage, &worktree, false)?,
                        Some(base.clone())
                    );
                    push_commit_to_branch(
                        &worktree,
                        remote_url,
                        stage,
                        &candidate,
                        Some(&base),
                        false,
                    )?;
                    assert_eq!(
                        resolve_remote_branch(remote_url, "sync", &worktree, false)?,
                        expected.map(str::to_owned)
                    );
                    validate_roundtrip(&candidate, &candidate)?;
                    Ok(candidate.clone())
                },
            )
            .unwrap();
            assert_eq!(
                resolve_remote_branch(remote_url, "sync", &worktree, false).unwrap(),
                Some(candidate.clone())
            );
            assert!(staging_refs(&remote).is_empty());
        }
    }

    #[test]
    fn conversion_and_validation_errors_leave_the_pr_branch_untouched() {
        let (_temp, worktree, remote) = test_repository();
        let base = commit_file(&worktree, "file", "base\n", "base");
        let previous = commit_file(&worktree, "file", "previous\n", "previous");
        let candidate = commit_file(&worktree, "file", "candidate\n", "candidate");
        let remote_url = remote.to_str().unwrap();
        for expected in [None, Some(previous.as_str())] {
            if let Some(sha) = expected {
                push_commit_to_branch(&worktree, remote_url, "sync", sha, None, false).unwrap();
            }
            for validation_error in [false, true] {
                let result = publish_staged_update(
                    &worktree,
                    remote_url,
                    "sync",
                    &base,
                    expected,
                    false,
                    |stage| {
                        if validation_error {
                            push_commit_to_branch(
                                &worktree,
                                remote_url,
                                stage,
                                &candidate,
                                Some(&base),
                                false,
                            )?;
                            validate_roundtrip(&candidate, &base)?;
                        }
                        anyhow::bail!("conversion failed")
                    },
                );
                assert!(result.is_err());
                assert_eq!(
                    resolve_remote_branch(remote_url, "sync", &worktree, false).unwrap(),
                    expected.map(str::to_owned)
                );
                assert!(staging_refs(&remote).is_empty());
            }
        }
    }

    #[test]
    fn refuses_concurrent_creation_change_or_deletion_during_conversion() {
        let (_temp, worktree, remote) = test_repository();
        let base = commit_file(&worktree, "file", "base\n", "base");
        let previous = commit_file(&worktree, "file", "previous\n", "previous");
        let candidate = commit_file(&worktree, "file", "candidate\n", "candidate");
        let remote_url = remote.to_str().unwrap();
        // The concurrent creation is a fast-forwardable ancestor of candidate.
        // A plain push would incorrectly accept it.
        for (expected, competing) in [
            (None, Some(base.as_str())),
            (Some(previous.as_str()), Some(base.as_str())),
            (Some(previous.as_str()), None),
        ] {
            git(&remote, &["update-ref", "-d", "refs/heads/sync"]);
            if let Some(sha) = expected {
                push_commit_to_branch(&worktree, remote_url, "sync", sha, None, false).unwrap();
            }
            let result = publish_staged_update(
                &worktree,
                remote_url,
                "sync",
                &base,
                expected,
                false,
                |stage| {
                    push_commit_to_branch(
                        &worktree,
                        remote_url,
                        stage,
                        &candidate,
                        Some(&base),
                        false,
                    )?;
                    match competing {
                        Some(sha) => {
                            git(&remote, &["update-ref", "refs/heads/sync", sha]);
                        }
                        None => {
                            git(&remote, &["update-ref", "-d", "refs/heads/sync"]);
                        }
                    }
                    Ok(candidate.clone())
                },
            );
            assert!(result.is_err());
            assert_eq!(
                resolve_remote_branch(remote_url, "sync", &worktree, false).unwrap(),
                competing.map(str::to_owned)
            );
            assert!(staging_refs(&remote).is_empty());
        }
    }

    #[test]
    fn staging_branches_are_unique_and_cleanup_is_independent() {
        let (_temp, worktree, remote) = test_repository();
        let base = commit_file(&worktree, "file", "base\n", "base");
        let first =
            RemoteStageBranch::create(&worktree, remote.to_str().unwrap(), &base, false).unwrap();
        let second =
            RemoteStageBranch::create(&worktree, remote.to_str().unwrap(), &base, false).unwrap();
        assert_ne!(first.branch, second.branch);
        assert_eq!(staging_refs(&remote).lines().count(), 2);
        drop(first);
        assert_eq!(staging_refs(&remote).lines().count(), 1);
        drop(second);
        assert!(staging_refs(&remote).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_failure_preserves_success_or_the_original_error() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, worktree, remote) = test_repository();
        let base = commit_file(&worktree, "file", "base\n", "base");
        let candidate = commit_file(&worktree, "file", "candidate\n", "candidate");
        let hook = remote.join("hooks/update");
        fs::write(&hook, "#!/bin/sh\ncase \"$1:$3\" in refs/heads/blueos-josh-stage-*:0000000000000000000000000000000000000000) exit 1;; esac\nexit 0\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let remote_url = remote.to_str().unwrap();
        publish_staged_update(&worktree, remote_url, "sync", &base, None, false, |_| {
            Ok(candidate.clone())
        })
        .unwrap();
        assert_eq!(
            resolve_remote_branch(remote_url, "sync", &worktree, false).unwrap(),
            Some(candidate.clone())
        );
        let error = publish_staged_update(
            &worktree,
            remote_url,
            "sync",
            &base,
            Some(&candidate),
            false,
            |_| anyhow::bail!("original validation error"),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "original validation error");
        assert_eq!(staging_refs(&remote).lines().count(), 2);
    }

    #[test]
    fn tree_comparison_includes_blueos_version() {
        let (_temp, worktree, _remote) = test_repository();
        let first = commit_file(&worktree, "file", "same\n", "first");
        git(
            worktree.as_path(),
            &["commit", "--allow-empty", "-qm", "empty"],
        );
        let same_tree = git(&worktree, &["rev-parse", "HEAD"]);
        let version_change =
            commit_file(&worktree, "blueos-version", "0123456789abcdef\n", "version");

        assert!(refs_have_same_tree(&first, &same_tree, &worktree, false).unwrap());
        assert!(!refs_have_same_tree(&same_tree, &version_change, &worktree, false).unwrap());
        assert!(matches!(
            ensure_push_needed(&first, &same_tree, &worktree, false),
            Err(BlueosPushError::NothingToPush)
        ));
        assert!(ensure_push_needed(&same_tree, &version_change, &worktree, false).is_ok());
    }

    #[test]
    fn rejects_a_non_roundtrip_push() {
        assert!(validate_roundtrip("expected", "expected").is_ok());
        let error = validate_roundtrip("expected", "actual")
            .unwrap_err()
            .to_string();
        assert!(error.contains("Expected expected, got actual"));
    }

    #[test]
    fn no_rev_block_unchanged() {
        assert_eq!(convert_rev_syntax(":/some/path"), ":/some/path");
    }

    #[test]
    fn single_sha_gets_prefix() {
        assert_eq!(
            convert_rev_syntax(":rev(3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)"),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)",
        );
    }

    #[test]
    fn null_sha_becomes_underscore() {
        assert_eq!(
            convert_rev_syntax(":rev(0000000000000000000000000000000000000000:/some/path)"),
            ":rev(_:/some/path)",
        );
    }

    #[test]
    fn multiple_entries_in_rev_block() {
        assert_eq!(
            convert_rev_syntax(
                ":rev(3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1,\
                 e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2,\
                 0000000000000000000000000000000000000000:/p3)"
            ),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1,\
             <=e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2,\
             _:/p3)",
        );
    }

    #[test]
    fn already_converted_syntax_unchanged() {
        assert_eq!(
            convert_rev_syntax(":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)"),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)",
        );
    }

    #[test]
    fn underscore_syntax_unchanged() {
        assert_eq!(
            convert_rev_syntax(":rev(_:/some/path)"),
            ":rev(_:/some/path)",
        );
    }

    #[test]
    fn sha_outside_rev_block_unchanged() {
        assert_eq!(
            convert_rev_syntax("3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path"),
            "3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path",
        );
    }

    #[test]
    fn wrap_compat_simple_filter() {
        assert_eq!(
            wrap_compat(":/some/path"),
            ":~(history=\"keep-trivial-merges\",gpgsig=\"norm-lf\")[:/some/path]",
        );
    }

    #[test]
    fn wrap_compat_rev_filter() {
        assert_eq!(
            wrap_compat(
                ":rev(75dd959a3a40eb5b4574f8d2e23aa6efbeb33573:prefix=src/tools/miri):/src/tools/miri"
            ),
            ":~(history=\"keep-trivial-merges\",gpgsig=\"norm-lf\")\
             [:rev(75dd959a3a40eb5b4574f8d2e23aa6efbeb33573:prefix=src/tools/miri):/src/tools/miri]",
        );
    }

    #[test]
    fn multiple_rev_blocks() {
        assert_eq!(
            convert_rev_syntax(
                ":rev(3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1)\
                 :rev(e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2)"
            ),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1)\
             :rev(<=e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2)",
        );
    }
}
