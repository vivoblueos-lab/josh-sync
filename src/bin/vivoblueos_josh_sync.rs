use anyhow::Context;
use clap::Parser;
use std::path::{Path, PathBuf};
use vivoblueos_josh_sync::SyncContext;
use vivoblueos_josh_sync::config::{JoshConfig, load_config};
use vivoblueos_josh_sync::josh::{JoshProxy, try_install_josh_proxy};
use vivoblueos_josh_sync::sync::{
    BluekernelPullError, BluekernelPushError, FilterVersion, GitSync, NO_REBASE_WARN,
};
use vivoblueos_josh_sync::utils::{get_current_head_sha, prompt};

const DEFAULT_CONFIG_PATH: &str = "josh-sync.toml";
const DEFAULT_BLUEKERNEL_VERSION_PATH: &str = "bluekernel-version";

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    cmd: Command,
}

#[derive(clap::Parser)]
enum Command {
    /// Initialize a config file and an empty `bluekernel-version` file for this repository.
    Init,
    /// Pull changes from the BlueKernel monorepo configured in `josh-sync.toml`.
    /// This creates new commits that should be then merged into this subtree repository.
    Pull {
        /// Override the configured upstream repository for a local experiment.
        /// CI should use the checked-in `upstream-repo` instead.
        #[clap(long)]
        upstream_repo: Option<String>,

        /// Override the configured upstream branch for a local experiment.
        /// CI should use the checked-in `upstream-branch` instead.
        #[clap(long)]
        upstream_branch: Option<String>,

        /// Override the BlueKernel monorepo commit that we should pull from.
        /// By default, josh-sync resolves the configured BlueKernel monorepo branch.
        #[clap(long)]
        upstream_commit: Option<String>,

        /// By default, the `pull` command will exit with status code 2 if there is nothing to pull,
        /// and reset git to the original state.
        /// If you instead want to exit successfully and keep the intermediate changes
        /// in that case, pass this flag.
        #[clap(long)]
        allow_noop: bool,
        #[clap(flatten)]
        shared: SharedArgs,
    },
    /// Push changes into `branch` of a fork of the configured BlueKernel monorepo under the given
    /// GitHub `username`.
    /// The pushed branch should then be merged into the BlueKernel monorepo.
    Push {
        /// Branch that should be pushed to your remote
        branch: String,

        /// Your GitHub usename where the fork is located
        username: String,

        /// Replace an existing sync branch using force-with-lease.
        /// This is intended for a CI-owned branch.
        #[clap(long)]
        update_existing: bool,
        #[clap(flatten)]
        shared: SharedArgs,
    },
}

#[derive(clap::Parser)]
struct SharedArgs {
    /// Path to the josh-sync TOML config file.
    #[clap(long, default_value(DEFAULT_CONFIG_PATH))]
    config_path: PathBuf,

    /// Path to a file storing the last synchronized BlueKernel monorepo commit.
    #[clap(long, default_value(DEFAULT_BLUEKERNEL_VERSION_PATH))]
    bluekernel_version_path: PathBuf,

    /// Path to the josh-proxy binary to be used.
    /// If not specified, it will be installed automatically.
    ///
    /// Warning: if you use a custom Josh version, ensure that it works properly!
    #[clap(long)]
    josh_proxy: Option<PathBuf>,

    /// Print executed commands.
    #[clap(long, short = 'v', env = "JOSH_SYNC_VERBOSE")]
    verbose: bool,

    /// Do not show confirmation prompts; use their safe defaults.
    #[clap(long)]
    no_interact: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.cmd {
        Command::Init => {
            let config = JoshConfig {
                org: "vivoblueos".to_string(),
                repo: "<repository-name>".to_string(),
                upstream_repo: "<github-owner>/<bluekernel-monorepo>".to_string(),
                upstream_branch: "main".to_string(),
                path: Some("<relative-subtree-path>".to_string()),
                filter: None,
                post_pull: vec![],
                subtree_filter: None,
                filter_version: FilterVersion::latest(),
            };
            config
                .write(Path::new(DEFAULT_CONFIG_PATH))
                .context("cannot write config")?;
            println!("Created config file at {DEFAULT_CONFIG_PATH}");

            if !Path::new(DEFAULT_BLUEKERNEL_VERSION_PATH).is_file() {
                std::fs::write(DEFAULT_BLUEKERNEL_VERSION_PATH, "")
                    .context("cannot write bluekernel-version file")?;
                println!(
                    "Created empty bluekernel-version file at {DEFAULT_BLUEKERNEL_VERSION_PATH}"
                );
            } else {
                println!(
                    "{DEFAULT_BLUEKERNEL_VERSION_PATH} already exists, not doing anything with it"
                );
            }
        }
        Command::Pull {
            upstream_repo,
            upstream_branch,
            upstream_commit,
            allow_noop,
            shared,
        } => {
            let ctx = load_context(&shared.config_path, &shared.bluekernel_version_path)?;
            let josh = get_josh_proxy(shared.josh_proxy, shared.verbose)?;
            let sync = GitSync::new(ctx.clone(), josh, shared.verbose);
            let upstream_repo = upstream_repo.unwrap_or_else(|| ctx.config.upstream_repo.clone());
            let upstream_branch =
                upstream_branch.unwrap_or_else(|| ctx.config.upstream_branch.clone());
            match sync.bluekernel_pull(upstream_repo, upstream_branch, upstream_commit, allow_noop)
            {
                Ok(result) => {
                    if !maybe_create_gh_pr(
                        &ctx.config.full_repo_name(),
                        "BlueKernel pull update",
                        &result.merge_commit_message,
                        shared.no_interact,
                    )? {
                        println!(
                            "Now push the current branch to {} (either a fork or the main repo) and create a PR",
                            ctx.config.repo
                        );
                    }
                }
                Err(BluekernelPullError::NothingToPull) => {
                    eprintln!("Nothing to pull");
                    if !allow_noop {
                        std::process::exit(2);
                    }
                }
                Err(BluekernelPullError::PullFailed(error)) => {
                    eprintln!("Pull failure: {error:?}");
                    if !shared.verbose {
                        eprintln!("Rerun with `-v` to see executed commands");
                    }
                    std::process::exit(1);
                }
            }
        }
        Command::Push {
            username,
            branch,
            update_existing,
            shared,
        } => {
            let ctx = load_context(&shared.config_path, &shared.bluekernel_version_path)?;
            let josh = get_josh_proxy(shared.josh_proxy, shared.verbose)?;
            let sync = GitSync::new(ctx.clone(), josh, shared.verbose);
            match sync.bluekernel_push(&username, &branch, update_existing, shared.no_interact) {
                Ok(()) => {}
                Err(BluekernelPushError::NothingToPush) => {
                    eprintln!("Nothing to push");
                    std::process::exit(2);
                }
                Err(BluekernelPushError::PushFailed(error)) => {
                    if !shared.verbose {
                        eprintln!("Rerun with `-v` to see executed commands");
                    }
                    return Err(error).context("cannot perform push");
                }
            }

            // Open PR with `subtree update` title to silence the `no-merges` triagebot check
            let title = format!("{} subtree update", ctx.config.repo);
            let head = get_current_head_sha(shared.verbose)?;

            let merge_msg = format!(
                r#"Subtree update of `{repo}` to https://github.com/{full_repo}/commit/{head}.

Created using vivoblueos-josh-sync.

{warning}"#,
                repo = ctx.config.repo,
                full_repo = ctx.config.full_repo_name(),
                warning = NO_REBASE_WARN
            );

            println!(
                r#"You can create the BlueKernel monorepo PR using the following URL:
https://github.com/{upstream_repo}/compare/{upstream_branch}...{username}:{branch}?quick_pull=1&title={}&body={}"#,
                urlencoding::encode(&title),
                urlencoding::encode(&merge_msg),
                upstream_repo = ctx.config.upstream_repo,
                upstream_branch = ctx.config.upstream_branch
            );
        }
    }

    Ok(())
}

fn load_context(config_path: &Path, bluekernel_version_path: &Path) -> anyhow::Result<SyncContext> {
    let config = load_config(config_path)
        .context("cannot load config. Run the `init` command to initialize it.")?;
    let bluekernel_version = std::fs::read_to_string(bluekernel_version_path)
        .inspect_err(|err| eprintln!("Cannot load bluekernel-version file: {err:?}"))
        .map(|version| version.trim().to_string())
        .map(Some)
        .unwrap_or_default();
    Ok(SyncContext {
        config,
        last_upstream_sha_path: bluekernel_version_path.to_path_buf(),
        last_upstream_sha: bluekernel_version,
    })
}

fn maybe_create_gh_pr(
    repo: &str,
    title: &str,
    description: &str,
    no_interact: bool,
) -> anyhow::Result<bool> {
    if which::which("gh").is_ok()
        && prompt(
            &format!("Do you want to create a {repo} pull PR using the `gh` tool?"),
            false,
            no_interact,
        )
    {
        std::process::Command::new("gh")
            .args([
                "pr",
                "create",
                "--title",
                title,
                "--body",
                description,
                "--repo",
                repo,
            ])
            .spawn()?
            .wait()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn get_josh_proxy(proxy_path: Option<PathBuf>, verbose: bool) -> anyhow::Result<JoshProxy> {
    match proxy_path {
        Some(path) => {
            println!("Using josh-proxy binary from {}", path.display());
            Ok(JoshProxy::from_path(path))
        }
        None => match try_install_josh_proxy(verbose) {
            Some(proxy) => Ok(proxy),
            None => Err(anyhow::anyhow!("Could not install josh-proxy")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Args, Command};
    use clap::Parser;

    #[test]
    fn push_accepts_no_interact() {
        let args = Args::try_parse_from([
            "vivoblueos-josh-sync",
            "push",
            "sync-branch",
            "example-owner",
            "--no-interact",
        ])
        .unwrap();

        let Command::Push { shared, .. } = args.cmd else {
            panic!("expected push command");
        };
        assert!(shared.no_interact);
    }
}
