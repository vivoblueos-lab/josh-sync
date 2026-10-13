use crate::sync::FilterVersion;
use anyhow::Context;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct JoshConfig {
    #[serde(default = "default_org")]
    pub org: String,
    pub repo: String,
    /// GitHub owner/name of the BlueKernel monorepo from which this subtree is synchronized.
    /// For example `Natural-selection1/bluekernel-mono` during a trial or `vivoblueos/bluekernel`
    /// in production.
    pub upstream_repo: String,
    /// Branch in the BlueKernel monorepo to synchronize.
    /// We resolve this branch to a commit SHA before Josh is invoked.
    pub upstream_branch: String,
    /// Relative path where the subtree is located in the BlueKernel monorepo.
    /// For example `src/doc/bluekernel-dev-guide`.
    pub path: Option<String>,
    /// Optional filter specification for Josh.
    /// It cannot be used together with `path`.
    pub filter: Option<String>,
    /// Operation(s) that should be performed after a pull.
    /// Can be used to post-process the state of the repository after a pull happens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_pull: Vec<PostPullOperation>,
    /// Optional subtree filter applied to the local `HEAD` during round-trip check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtree_filter: Option<String>,
    /// Optional filter version that determines which post-processing will be applied to the
    /// specified filter.
    ///
    /// This exists for backwards compatibility with repositories using an older filter syntax.
    #[serde(
        default = "default_filter_version",
        skip_serializing_if = "skip_serializing_filter_version",
        with = "filter_version"
    )]
    pub filter_version: FilterVersion,
}

impl JoshConfig {
    pub fn full_repo_name(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let config = toml::to_string_pretty(self).context("cannot serialize config")?;
        std::fs::write(path, config).context("cannot write config")?;
        Ok(())
    }

    /// Return the name of the configured monorepo, without its GitHub owner.
    /// This is used to locate a contributor's fork during a reverse push.
    pub fn upstream_repo_name(&self) -> anyhow::Result<&str> {
        let mut parts = self.upstream_repo.split('/');
        let owner = parts.next();
        let repo = parts.next();
        if owner.is_none_or(str::is_empty)
            || repo.is_none_or(str::is_empty)
            || parts.next().is_some()
        {
            return Err(anyhow::anyhow!(
                "`upstream-repo` must be a GitHub owner/repository pair, got `{}`",
                self.upstream_repo
            ));
        }
        Ok(repo.expect("validated above"))
    }
}

mod filter_version {
    use crate::sync::FilterVersion;
    use serde::de::{Error, Unexpected};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        version: &FilterVersion,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let num: u32 = match version {
            FilterVersion::Version1 => 1,
            FilterVersion::Version2 => 2,
        };
        num.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<FilterVersion, D::Error> {
        let num = u32::deserialize(deserializer)?;
        match num {
            1 => Ok(FilterVersion::Version1),
            2 => Ok(FilterVersion::Version2),
            v => Err(D::Error::invalid_value(
                Unexpected::Unsigned(v as u64),
                &"1 or 2",
            )),
        }
    }
}

fn default_filter_version() -> FilterVersion {
    FilterVersion::Version1
}

fn skip_serializing_filter_version(version: &FilterVersion) -> bool {
    match version {
        FilterVersion::Version1 => true,
        FilterVersion::Version2 => false,
    }
}

/// Execute an operation after a pull, and if something changes in the local git state,
/// perform a commit.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct PostPullOperation {
    /// Execute a command with these arguments
    /// At least one argument has to be present.
    /// You can run e.g. bash if you want to do more complicated stuff.
    pub cmd: Vec<String>,
    /// If the git state has changed after `cmd`, add all changes to the index (`git add -u`)
    /// and create a commit with the following commit message.
    pub commit_message: String,
}

fn default_org() -> String {
    String::from("vivoblueos")
}

pub fn load_config(path: &Path) -> anyhow::Result<JoshConfig> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("cannot load config file from {}", path.display()))?;
    let config: JoshConfig = toml::from_str(&data).context("cannot load config as TOML")?;
    if config.path.is_some() == config.filter.is_some() {
        return if config.path.is_some() {
            Err(anyhow::anyhow!("Cannot specify both `path` and `filter`"))
        } else {
            Err(anyhow::anyhow!("Must specify one of `path` and `filter`"))
        };
    }

    config.upstream_repo_name()?;
    if config.upstream_branch.trim().is_empty() {
        return Err(anyhow::anyhow!("`upstream-branch` must not be empty"));
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(upstream_repo: &str) -> JoshConfig {
        JoshConfig {
            org: "Natural-selection1".to_string(),
            repo: "kernel".to_string(),
            upstream_repo: upstream_repo.to_string(),
            upstream_branch: "main".to_string(),
            path: Some("kernel".to_string()),
            filter: None,
            post_pull: vec![],
            subtree_filter: None,
            filter_version: FilterVersion::Version2,
        }
    }

    #[test]
    fn obtains_the_monorepo_name_from_a_github_repository() {
        assert_eq!(
            config("Natural-selection1/bluekernel-mono")
                .upstream_repo_name()
                .unwrap(),
            "bluekernel-mono"
        );
    }

    #[test]
    fn rejects_an_invalid_monorepo_repository() {
        assert!(config("bluekernel-mono").upstream_repo_name().is_err());
        assert!(config("owner/repo/extra").upstream_repo_name().is_err());
    }
}
