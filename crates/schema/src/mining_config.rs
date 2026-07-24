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
}
