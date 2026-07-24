//! Non-secret config for `autoreview auth` — the *actual* credential
//! never lives here (`.autoreview/config.yaml` is committed to git and
//! meant to be reviewed, per `docs/autoreview-directory-layout.md`; a
//! real secret belongs in the OS keyring or a machine-local file, see
//! `autoreview_core::auth::credential_store`). Only pointer/policy data
//! safe to share with the whole team goes here.
//!
//! A flat struct with one field per provider (`AuthConfig{github,
//! bitbucket}`), not a tagged enum like `RulePackSourceConfig` — GitHub
//! and Bitbucket aren't mutually-exclusive alternatives for one slot, a
//! user configures both independently, the same shape `AgentsConfig
//! {local_llm, embedding}` already uses for its own two independently-
//! usable backends.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfig {
    #[serde(default)]
    pub github: GithubAuthConfig,
    #[serde(default)]
    pub bitbucket: BitbucketAuthConfig,
}

/// No compiled-in default `client_id` — see
/// `autoreview_core::auth::github_device_flow`'s module doc for why:
/// registering a GitHub OAuth App with Device Flow enabled is a one-time
/// action only a maintainer with a GitHub account can take, not something
/// this project can generate for itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubAuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// Empty today — Bitbucket's per-login account (email) is resolved and
/// remembered at login time (`CredentialStore::remember_account`), not
/// configured up front. Kept as its own struct (not omitted from
/// `AuthConfig` entirely) so a future repo-level Bitbucket setting (e.g.
/// a default workspace) has an obvious home to land in without a schema
/// reshape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BitbucketAuthConfig {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_no_client_id_when_the_section_is_absent() {
        let parsed: AuthConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(parsed.github.client_id, None);
    }

    #[test]
    fn round_trips_an_explicit_client_id() {
        let parsed: AuthConfig = serde_yaml::from_str("github:\n  clientId: Iv1.abc123\n").unwrap();
        assert_eq!(parsed.github.client_id, Some("Iv1.abc123".to_string()));
    }
}
