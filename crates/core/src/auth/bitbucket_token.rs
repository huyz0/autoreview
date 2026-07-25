//! Bitbucket Cloud login: HTTP Basic auth with an Atlassian **API
//! token** — the current, non-deprecated mechanism (Atlassian is fully
//! removing the older "app passwords" by 2026-07-28). Unlike GitHub's
//! device flow, there's no OAuth polling dance to build here: an API
//! token is created manually by the user at `id.atlassian.com`, so this
//! module's job is just verifying a pasted token is real (and telling the
//! user clearly if it isn't) before storing it.
//!
//! HTTP Basic's username slot is the account's **email address**, not a
//! username — Bitbucket Cloud now requires this explicitly for API-token
//! auth (a bare username no longer works). This is also why
//! `auth::CredentialStore`'s "account" for the Bitbucket service is an
//! email, not a fixed constant the way GitHub's is.

use std::process::Command;

use serde::Deserialize;

/// The handful of `GET /2.0/user` response fields this module actually
/// uses — Bitbucket's real response also includes `type`/`uuid`/
/// `nickname`/`created_on`, not modeled here since nothing needs them.
/// `username`-based identification is Atlassian's own deprecated path
/// (superseded by `account_id`), so this deliberately doesn't attempt to
/// read it.
#[derive(Debug, Deserialize)]
pub struct BitbucketUser {
    pub account_id: String,
    pub display_name: String,
}

/// Takes the binary rather than hardcoding `"curl"`: the caller already
/// accepts a configurable `curl_binary`, and it previously reached only
/// this function's error messages while the invocation itself ignored it.
fn run_curl(curl_binary: &str, args: &[&str]) -> anyhow::Result<(u32, String)> {
    let output = Command::new(curl_binary).args(args).output()?;
    if !output.status.success() {
        anyhow::bail!("curl failed to run: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // The last line is the status code, appended by `-w '\n%{http_code}'`
    // below — everything before it is the response body.
    let mut lines: Vec<&str> = stdout.lines().collect();
    let status_line = lines.pop().unwrap_or_default();
    let status: u32 = status_line.trim().parse().unwrap_or(0);
    Ok((status, lines.join("\n")))
}

pub(crate) fn parse_bitbucket_user(body: &str) -> anyhow::Result<BitbucketUser> {
    Ok(serde_json::from_str(body)?)
}

/// Verifies `email`/`api_token` against Bitbucket Cloud's real API —
/// `GET /2.0/user` via HTTP Basic auth. The credential goes through a
/// `0600` curl config file (`auth::curl_config`) rather than a `-u` argv
/// entry: an argument is visible to every other user on the machine via
/// `ps`/`/proc/<pid>/cmdline` for the life of the request. `--max-time 15`
/// since this is a real internet call, unlike the existing
/// `localhost`-only curl call sites in `agents::local_llm`/
/// `agents::embedding`, which don't need one.
pub fn verify_bitbucket_token(email: &str, api_token: &str, curl_binary: &str) -> anyhow::Result<BitbucketUser> {
    let auth_config = super::curl_config::CurlAuthConfig::basic(email, api_token).map_err(|err| anyhow::anyhow!("failed to stage curl credentials: {err}"))?;
    let config_path = auth_config.path().display().to_string();
    let (status, body) = run_curl(curl_binary, &["-sS", "--config", &config_path, "-w", "\n%{http_code}", "--max-time", "15", "https://api.bitbucket.org/2.0/user"])
        .map_err(|err| anyhow::anyhow!("failed to reach Bitbucket ({curl_binary} error): {err}"))?;

    if status == 401 || status == 403 {
        anyhow::bail!("Bitbucket rejected this email/API token combination (HTTP {status}) — check the token was created for {email} and hasn't expired");
    }
    if status != 200 {
        anyhow::bail!("Bitbucket returned an unexpected status (HTTP {status}) while verifying the token");
    }

    parse_bitbucket_user(&body).map_err(|err| anyhow::anyhow!("Bitbucket's response didn't look like the expected user shape: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_shaped_user_response() {
        let body = r#"{"type":"user","uuid":"{abc123}","account_id":"557058:abc","display_name":"Jane Doe","nickname":"janedoe","created_on":"2020-01-01T00:00:00Z"}"#;
        let user = parse_bitbucket_user(body).unwrap();
        assert_eq!(user.account_id, "557058:abc");
        assert_eq!(user.display_name, "Jane Doe");
    }

    #[test]
    fn errors_clearly_on_a_response_missing_expected_fields() {
        let err = parse_bitbucket_user(r#"{"error":{"message":"Invalid credentials"}}"#).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("missing field") || err.to_string().contains("account_id"), "got: {err}");
    }

    #[test]
    fn errors_clearly_on_malformed_json() {
        assert!(parse_bitbucket_user("not json").is_err());
    }

    /// Regression test: `curl_binary` used to reach only the error
    /// message while the invocation hardcoded `"curl"`, so configuring it
    /// silently did nothing. Passing a binary that cannot exist must fail
    /// at the spawn step — if this ever passes by reaching the real
    /// Bitbucket API again, the parameter has been disconnected a second
    /// time.
    #[test]
    fn the_configured_curl_binary_is_actually_invoked() {
        let err = verify_bitbucket_token("fake@example.com", "irrelevant", "definitely-not-a-real-binary-xyz").unwrap_err();
        assert!(err.to_string().contains("failed to reach Bitbucket"), "got: {err}");
        assert!(err.to_string().contains("definitely-not-a-real-binary-xyz"), "got: {err}");
    }

    #[test]
    fn a_bad_token_is_clearly_rejected_by_the_real_bitbucket_api() {
        // A real network call against the live Bitbucket API with a
        // deliberately invalid credential — no real account needed,
        // confirms the 401 branch actually fires against Bitbucket's
        // real response shape rather than a shape assumed from docs.
        // Best-effort: skipped, not failed, if this environment has no
        // outbound network access at all ("failed to reach Bitbucket" is
        // this module's own wording for a curl-level connection failure,
        // distinct from a real 401/403 rejection).
        match verify_bitbucket_token("fake@example.com", "definitely-not-a-real-token", "curl") {
            Err(err) if err.to_string().contains("rejected") => {}
            Err(err) if err.to_string().contains("failed to reach Bitbucket") => {
                eprintln!("skipping: no network access in this environment ({err})");
            }
            Err(err) => panic!("got an unexpected error shape: {err}"),
            Ok(user) => panic!("expected the fake credential to be rejected, got: {user:?}"),
        }
    }
}
