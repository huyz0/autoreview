//! Config sections for the newer rule-mining sources (see
//! `crates/core/src/rule_factory/`) — kept in their own file rather than
//! growing `config.rs` further, one struct per source, following
//! `MineFromCommentsConfig`'s own shape (`enabled` opt-in where the
//! source touches a network/external tool, plain defaulted fields
//! otherwise).

use serde::{Deserialize, Serialize};

fn default_max_bugfix_commits() -> usize {
    200
}

/// `mineFromBugfixCommits` — no `enabled` gate, unlike the network-backed
/// sources: this only ever reads the repo's own local `git log`/`git
/// show`, the same "always available, no opt-in needed" posture
/// `--from-code`'s call-pair mining already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MineFromBugfixCommitsConfig {
    /// How many of the repo's most recent commits to scan for a bug-fix-
    /// shaped subject line — a fixed count, not a date window, for the
    /// same reason `MineFromCommentsConfig::lookback_prs` is: a bounded,
    /// predictable amount of `git` subprocess work regardless of how old
    /// or active the repo is.
    #[serde(default = "default_max_bugfix_commits")]
    pub max_commits: usize,
}

impl Default for MineFromBugfixCommitsConfig {
    fn default() -> Self {
        MineFromBugfixCommitsConfig { max_commits: default_max_bugfix_commits() }
    }
}

fn default_lookback_prs() -> usize {
    30
}

fn default_curl_binary() -> String {
    "curl".to_string()
}

/// `mineFromBitbucketComments` — opt-in (`enabled: false` by default),
/// the same posture `MineFromCommentsConfig` (GitHub) already takes for
/// any source that touches a real external API. `workspace` is
/// repo-level, shared policy safe to commit (unlike the Bitbucket
/// credential itself, which never belongs here — see
/// `autoreview_core::auth::credential_store`); left `None` to fall back
/// to `mine_from_bitbucket_comments::resolve_bitbucket_repo_slug`
/// inferring it from `origin`'s remote URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MineFromBitbucketCommentsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_lookback_prs")]
    pub lookback_prs: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default = "default_curl_binary")]
    pub curl_binary: String,
}

impl Default for MineFromBitbucketCommentsConfig {
    fn default() -> Self {
        MineFromBitbucketCommentsConfig { enabled: false, lookback_prs: default_lookback_prs(), workspace: None, curl_binary: default_curl_binary() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_max_commits_to_200_when_the_section_is_absent() {
        let parsed: MineFromBugfixCommitsConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(parsed.max_commits, 200);
    }

    #[test]
    fn round_trips_an_explicit_max_commits() {
        let parsed: MineFromBugfixCommitsConfig = serde_yaml::from_str("maxCommits: 50").unwrap();
        assert_eq!(parsed.max_commits, 50);
    }

    #[test]
    fn bitbucket_comments_defaults_to_disabled_with_no_workspace() {
        let parsed: MineFromBitbucketCommentsConfig = serde_yaml::from_str("{}").unwrap();
        assert!(!parsed.enabled);
        assert_eq!(parsed.workspace, None);
        assert_eq!(parsed.lookback_prs, 30);
        assert_eq!(parsed.curl_binary, "curl");
    }

    #[test]
    fn bitbucket_comments_round_trips_an_explicit_workspace() {
        let parsed: MineFromBitbucketCommentsConfig = serde_yaml::from_str("enabled: true\nworkspace: my-team\nlookbackPrs: 10\n").unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.workspace, Some("my-team".to_string()));
        assert_eq!(parsed.lookback_prs, 10);
    }
}
