//! GitHub login via the OAuth **Device Flow** (RFC 8628) — chosen over
//! delegating to `gh auth login` (as `mine_from_comments.rs` already does
//! for its own GitHub API calls) so this project stores the token itself
//! the same way it stores a Bitbucket one, rather than depending on a
//! second CLI being installed and pre-authenticated.
//!
//! **The `client_id` is not a secret** — this is the actual security
//! model device flow is built for, not a corner this module cuts. RFC
//! 8628's `client_id` identifies *which app is asking*, not a credential
//! proving the app's authenticity; it's meant to be embedded in a public
//! client (a CLI, exactly like this one) with no confidential secret at
//! all. Security rests entirely on the human confirming a short
//! `user_code` at GitHub's own real, TLS-protected login page — this
//! module never touches the user's GitHub password. This is the same
//! model `gh` itself uses.
//!
//! **This module has no compiled-in `client_id`**, unlike a real shipped
//! product would — registering a GitHub OAuth App with Device Flow
//! enabled is a one-time action in some GitHub account/org's Developer
//! Settings that only a maintainer with such an account can take, not
//! something this code can generate for itself. Configure one via
//! `auth.github.clientId` in `.autoreview/config.yaml`; `run_auth_login`
//! (the CLI layer) errors clearly, pointing at GitHub's own device-flow
//! docs, when it's absent.
//!
//! Scope requested is `repo` (not the narrower `public_repo`) since
//! reading PR review comments on a private repository — the realistic
//! common case for a real engineering org — needs it; `public_repo`
//! would silently fail for exactly the repos most worth mining.

use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PollOutcome {
    Success { access_token: String },
    Pending,
    SlowDown,
    Denied,
    Expired,
}

#[derive(Debug, Deserialize)]
struct RawErrorResponse {
    error: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawPollResponse {
    access_token: Option<String>,
    error: Option<String>,
}

fn run_curl(args: &[&str]) -> anyhow::Result<(u32, String)> {
    let output = Command::new("curl").args(args).output()?;
    if !output.status.success() {
        anyhow::bail!("curl failed to run: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines: Vec<&str> = stdout.lines().collect();
    let status_line = lines.pop().unwrap_or_default();
    let status: u32 = status_line.trim().parse().unwrap_or(0);
    Ok((status, lines.join("\n")))
}

pub(crate) fn parse_device_code_response(body: &str) -> anyhow::Result<DeviceCodeResponse> {
    if let Ok(resp) = serde_json::from_str::<DeviceCodeResponse>(body) {
        return Ok(resp);
    }
    if let Ok(err) = serde_json::from_str::<RawErrorResponse>(body) {
        anyhow::bail!("GitHub rejected the device-code request: {}", err.error);
    }
    anyhow::bail!("GitHub's device-code response didn't look like the expected shape: {body}");
}

pub(crate) fn parse_poll_response(body: &str) -> anyhow::Result<PollOutcome> {
    let raw: RawPollResponse = serde_json::from_str(body)?;
    if let Some(access_token) = raw.access_token {
        return Ok(PollOutcome::Success { access_token });
    }
    match raw.error.as_deref() {
        Some("authorization_pending") => Ok(PollOutcome::Pending),
        Some("slow_down") => Ok(PollOutcome::SlowDown),
        Some("expired_token") => Ok(PollOutcome::Expired),
        Some("access_denied") => Ok(PollOutcome::Denied),
        Some(other) => anyhow::bail!("GitHub returned an unrecognized device-flow response: {other}"),
        None => anyhow::bail!("GitHub's poll response had neither an access_token nor an error field: {body}"),
    }
}

/// `POST https://github.com/login/device/code` — `--data-urlencode` (curl's
/// own flag) handles form-encoding, no percent-encoding crate needed.
pub fn request_device_code(client_id: &str, scope: &str, curl_binary: &str) -> anyhow::Result<DeviceCodeResponse> {
    let (_status, body) = run_curl(&[
        "-sS",
        "-X",
        "POST",
        "-H",
        "Accept: application/json",
        "--data-urlencode",
        &format!("client_id={client_id}"),
        "--data-urlencode",
        &format!("scope={scope}"),
        "--max-time",
        "15",
        "-w",
        "\n%{http_code}",
        "https://github.com/login/device/code",
    ])
    .map_err(|err| anyhow::anyhow!("failed to reach GitHub ({curl_binary} error): {err}"))?;
    parse_device_code_response(&body)
}

/// One poll of `POST https://github.com/login/oauth/access_token` — the
/// caller (`run_device_flow`) is responsible for the sleep-and-retry loop
/// this is meant to sit inside; kept as its own function so the loop
/// logic and the network call are independently testable/reasoned-about,
/// same split as every other network-touching module in this codebase.
pub fn poll_access_token_once(client_id: &str, device_code: &str, curl_binary: &str) -> anyhow::Result<PollOutcome> {
    let (_status, body) = run_curl(&[
        "-sS",
        "-X",
        "POST",
        "-H",
        "Accept: application/json",
        "--data-urlencode",
        &format!("client_id={client_id}"),
        "--data-urlencode",
        &format!("device_code={device_code}"),
        "--data-urlencode",
        "grant_type=urn:ietf:params:oauth:grant-type:device_code",
        "--max-time",
        "15",
        "-w",
        "\n%{http_code}",
        "https://github.com/login/oauth/access_token",
    ])
    .map_err(|err| anyhow::anyhow!("failed to reach GitHub ({curl_binary} error): {err}"))?;
    parse_poll_response(&body)
}

/// The interactive driver: requests a device code, prints the
/// verification URL + user code to `out`, then polls until success,
/// denial, or expiry — widening the poll interval by 5s on `slow_down`,
/// per the device-flow protocol. Returns the access token on success.
pub fn run_device_flow(client_id: &str, scope: &str, curl_binary: &str, out: &mut dyn Write) -> anyhow::Result<String> {
    let device = request_device_code(client_id, scope, curl_binary)?;
    writeln!(out, "To authorize, open {} and enter this code: {}", device.verification_uri, device.user_code)?;
    writeln!(out, "Waiting for authorization...")?;

    let deadline = Instant::now() + Duration::from_secs(device.expires_in);
    let mut interval = Duration::from_secs(device.interval);
    loop {
        std::thread::sleep(interval);
        if Instant::now() >= deadline {
            anyhow::bail!("device code expired before authorization completed — run `autoreview auth login github` again");
        }
        match poll_access_token_once(client_id, &device.device_code, curl_binary)? {
            PollOutcome::Success { access_token } => return Ok(access_token),
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => interval += Duration::from_secs(5),
            PollOutcome::Denied => anyhow::bail!("authorization was denied"),
            PollOutcome::Expired => anyhow::bail!("device code expired before authorization completed — run `autoreview auth login github` again"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_shaped_device_code_response() {
        let body = r#"{"device_code":"abc123","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;
        let resp = parse_device_code_response(body).unwrap();
        assert_eq!(resp.device_code, "abc123");
        assert_eq!(resp.user_code, "WDJB-MJHT");
        assert_eq!(resp.interval, 5);
    }

    #[test]
    fn parse_device_code_response_surfaces_a_real_error_shape_clearly() {
        // Confirmed against the real GitHub API with an invalid
        // client_id: {"error":"Not Found"}, HTTP 404.
        let err = parse_device_code_response(r#"{"error":"Not Found"}"#).unwrap_err();
        assert!(err.to_string().contains("Not Found"), "got: {err}");
    }

    #[test]
    fn parse_device_code_response_errors_clearly_on_an_unrecognized_shape() {
        assert!(parse_device_code_response("not json").is_err());
    }

    #[test]
    fn parses_a_successful_poll_response() {
        let outcome = parse_poll_response(r#"{"access_token":"gho_abc123","token_type":"bearer","scope":"repo"}"#).unwrap();
        assert_eq!(outcome, PollOutcome::Success { access_token: "gho_abc123".to_string() });
    }

    #[test]
    fn parses_every_documented_poll_error_shape() {
        assert_eq!(parse_poll_response(r#"{"error":"authorization_pending"}"#).unwrap(), PollOutcome::Pending);
        assert_eq!(parse_poll_response(r#"{"error":"slow_down"}"#).unwrap(), PollOutcome::SlowDown);
        assert_eq!(parse_poll_response(r#"{"error":"expired_token"}"#).unwrap(), PollOutcome::Expired);
        assert_eq!(parse_poll_response(r#"{"error":"access_denied"}"#).unwrap(), PollOutcome::Denied);
    }

    #[test]
    fn parse_poll_response_errors_clearly_on_an_unrecognized_error_code() {
        let err = parse_poll_response(r#"{"error":"something_new_github_added"}"#).unwrap_err();
        assert!(err.to_string().contains("something_new_github_added"), "got: {err}");
    }

    #[test]
    fn parse_poll_response_errors_clearly_when_neither_token_nor_error_is_present() {
        assert!(parse_poll_response(r#"{"unrelated":"field"}"#).is_err());
    }

    #[test]
    fn a_bad_client_id_is_clearly_rejected_by_the_real_github_api() {
        // Real network call against the live GitHub device-code endpoint
        // with a deliberately invalid client_id — no registered OAuth
        // App needed, confirms this module's error-parsing matches
        // GitHub's real response shape rather than one assumed from
        // docs. Best-effort: skipped, not failed, with no network access.
        match request_device_code("fake0000000000000000", "repo", "curl") {
            Err(err) if err.to_string().contains("rejected") => {}
            Err(err) if err.to_string().contains("failed to reach GitHub") => {
                eprintln!("skipping: no network access in this environment ({err})");
            }
            Err(err) => panic!("got an unexpected error shape: {err}"),
            Ok(resp) => panic!("expected the fake client_id to be rejected, got: {resp:?}"),
        }
    }
}
