//! Generic OpenAI-compatible hosted backend — same `/v1/chat/completions`
//! wire format `local_llm` already speaks (reuses its
//! `parse_chat_completion_response` unchanged, since that parser was
//! already provider-agnostic), but for a real internet host — OpenRouter
//! by default, or any other OpenAI-compatible provider (Together, Groq,
//! Fireworks, ...) via `base_url` — rather than localhost. Unlike
//! `local_llm` this needs a real secret (`Authorization: Bearer <token>`,
//! sourced from `auth::credential_store::CredentialStore`, never
//! hand-built into a logged command string) and a real timeout on every
//! call, since it's talking to the actual internet, not a process on this
//! machine.
//!
//! Same scope limitation as `local_llm`, stated plainly rather than
//! glossed over: single-shot, no tool access — `allowed_tools`/`max_turns`
//! on the request are ignored, same as `local_llm`. A real tool-calling
//! loop (OpenAI's function-calling contract, executed ourselves) would let
//! this backend do agentic-quality review the way `claude`/`pi` already
//! can — deliberately deferred as a follow-up, not built here.
//!
//! Also worth restating plainly: choosing this backend sends whole
//! diff/context content to a third-party host, a materially bigger trust
//! boundary than `local_llm`'s localhost-only default. Selecting
//! `agents.backend: openai-compatible` at all is the opt-in; there's no
//! separate flag layered on top of that choice.

use std::process::Command;
use std::time::Instant;

use super::claude_code::{truncate, AgentBackend, InvokeRequest, InvokeResult};
use super::local_llm::parse_chat_completion_response;

/// Bounds how long a single hosted-API call is allowed to hang before
/// `curl` gives up — real internet call, unlike `local_llm`'s
/// localhost-only requests, which have never needed one.
const REQUEST_TIMEOUT_SECONDS: &str = "120";

pub struct OpenAiCompatibleBackend {
    /// Base URL up to and including `/v1`, e.g.
    /// `https://openrouter.ai/api/v1` — `/chat/completions` is appended.
    pub base_url: String,
    pub api_key: String,
    pub curl_binary: String,
}

impl AgentBackend for OpenAiCompatibleBackend {
    fn invoke(&self, req: &InvokeRequest) -> anyhow::Result<InvokeResult> {
        let start = Instant::now();
        let body = serde_json::json!({
            "model": req.model,
            "messages": [
                {"role": "system", "content": req.system_prompt},
                {"role": "user", "content": req.prompt},
            ],
            "stream": false,
        });

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let auth_header = format!("Authorization: Bearer {}", self.api_key);
        let output = Command::new(&self.curl_binary)
            .args(["-sS", "-X", "POST", &url, "--max-time", REQUEST_TIMEOUT_SECONDS, "-H", "Content-Type: application/json", "-H", &auth_header, "-d", "@-"])
            .current_dir(&req.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(stdin) = child.stdin.take() {
                    let mut stdin = stdin;
                    let _ = stdin.write_all(body.to_string().as_bytes());
                }
                child.wait_with_output()
            })?;
        let wall_ms = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("openai-compatible request failed: {}", if stderr.trim().is_empty() { "(no stderr captured)" } else { stderr.trim() });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let (final_text, usage) = parse_chat_completion_response(&stdout)
            .map_err(|err| anyhow::anyhow!("openai-compatible response did not match the OpenAI chat-completions contract: {err}\nraw response: {}", truncate(&stdout, 500)))?;
        Ok(InvokeResult { final_text, usage, wall_ms })
    }
}

/// Reachability check — hits `{base_url}/models`. Deliberately does
/// **not** claim to verify `api_key` is actually valid: confirmed against
/// the real OpenRouter API that `/models` is public and returns 200 with
/// *any* bearer token, or none at all — only `/chat/completions` actually
/// enforces auth there, and likely on other providers too, since a public
/// model listing is a common, reasonable thing for a provider to expose
/// unauthenticated. This still returns `false` outright when no key is
/// configured at all, so it stays a meaningful "is this backend even
/// worth attempting" signal for `backend_available`'s cheap, frequent
/// (every Stage-3 dispatch) gate — see `verify_api_key` for the real,
/// one-time key check `auth login` runs instead.
pub fn openai_compatible_available(base_url: &str, api_key: &str, curl_binary: &str) -> bool {
    if api_key.is_empty() {
        return false;
    }
    let auth_header = format!("Authorization: Bearer {api_key}");
    Command::new(curl_binary)
        .args(["-sS", "-f", "-o", "/dev/null", "--max-time", "5", "-H", &auth_header, &format!("{}/models", base_url.trim_end_matches('/'))])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_curl_with_status(args: &[&str]) -> anyhow::Result<(u32, String)> {
    let output = Command::new("curl").args(args).output()?;
    if !output.status.success() {
        anyhow::bail!("curl failed to run: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // The last line is the status code, appended by `-w '\n%{http_code}'`
    // below — everything before it is the response body. Same idiom
    // `auth::bitbucket_token::run_curl` already uses, for the same reason:
    // `-f` would discard the error body we need to surface to the user.
    let mut lines: Vec<&str> = stdout.lines().collect();
    let status_line = lines.pop().unwrap_or_default();
    let status: u32 = status_line.trim().parse().unwrap_or(0);
    Ok((status, lines.join("\n")))
}

/// Makes one minimal real chat-completion request (`max_tokens: 1`) to
/// confirm `api_key` is actually accepted at `{base_url}/chat/completions`
/// — the one endpoint every OpenAI-compatible provider this backend
/// targets genuinely enforces authentication on (unlike `/models`, see
/// `openai_compatible_available`'s doc comment for the real API response
/// that disproved relying on it). This incurs a small real cost, unlike
/// every other check in this module — acceptable since it only runs once,
/// at `auth login` time, not on every Stage-3 dispatch, and it's the only
/// reliable way to actually catch a bad key before it's stored.
pub fn verify_api_key(base_url: &str, model: &str, api_key: &str, curl_binary: &str) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
        "stream": false,
    })
    .to_string();
    let auth_header = format!("Authorization: Bearer {api_key}");
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let (status, response_body) = run_curl_with_status(&["-sS", "-X", "POST", &url, "--max-time", "20", "-H", "Content-Type: application/json", "-H", &auth_header, "-w", "\n%{http_code}", "-d", &body])
        .map_err(|err| anyhow::anyhow!("failed to reach {base_url} ({curl_binary} error): {err}"))?;

    if status == 401 || status == 403 {
        anyhow::bail!("the provider rejected this API key (HTTP {status}) — response: {}", truncate(response_body.trim(), 300));
    }
    if status == 0 {
        anyhow::bail!("got no valid HTTP status back from {url} — check baseUrl in .autoreview/config.yaml");
    }
    if status >= 400 {
        anyhow::bail!(
            "{url} returned HTTP {status} for a minimal test request with model '{model}' — this may mean the key is fine but the configured model isn't available; response: {}",
            truncate(response_body.trim(), 300)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_compatible_available_is_false_with_no_api_key() {
        assert!(!openai_compatible_available("https://openrouter.ai/api/v1", "", "curl"));
    }

    #[test]
    fn openai_compatible_available_is_false_when_curl_itself_is_missing() {
        assert!(!openai_compatible_available("https://openrouter.ai/api/v1", "sk-fake", "definitely-not-a-real-binary-xyz"));
    }

    /// Regression precedent from `local_llm_available`'s own test suite:
    /// a plain `curl` without `--fail` treats any completed HTTP response
    /// (including a 401/404) as success (exit 0). Confirms this backend's
    /// check inherits the `-f` flag that closes that gap, against a real
    /// TCP listener answering 401 the way an invalid-token response would.
    #[test]
    fn openai_compatible_available_is_false_against_a_server_that_only_returns_401() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let available = openai_compatible_available(&format!("http://127.0.0.1:{port}"), "sk-fake", "curl");
        handle.join().unwrap();
        assert!(!available, "a 401-only server must not be reported as available");
    }

    #[test]
    fn invoke_reports_a_clear_error_when_the_request_itself_fails() {
        let backend = OpenAiCompatibleBackend { base_url: "http://127.0.0.1:1".to_string(), api_key: "sk-fake".to_string(), curl_binary: "curl".to_string() };
        let req = InvokeRequest {
            prompt: "hello".to_string(),
            system_prompt: "system".to_string(),
            allowed_tools: vec![],
            max_turns: 1,
            model: "openrouter/auto".to_string(),
            cwd: std::env::temp_dir(),
        };
        match backend.invoke(&req) {
            Err(err) => assert!(err.to_string().contains("openai-compatible request failed"), "got: {err}"),
            Ok(_) => panic!("expected an error connecting to a port nothing is listening on"),
        }
    }

    /// Deterministic regression test for the exact real-API behavior that
    /// motivated `verify_api_key` in the first place: a fake server
    /// standing in for a provider that actually enforces auth (unlike
    /// `/models`, confirmed public against the real OpenRouter API) and
    /// rejects a bad key with a real 401 body.
    #[test]
    fn verify_api_key_reports_a_clear_error_on_a_401() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let body = r#"{"error":{"message":"Invalid API key","code":401}}"#;
                let response = format!("HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let result = verify_api_key(&format!("http://127.0.0.1:{port}"), "openrouter/auto", "sk-fake", "curl");
        handle.join().unwrap();
        let err = match result {
            Err(err) => err,
            Ok(()) => panic!("expected the fake 401 response to be reported as rejected"),
        };
        assert!(err.to_string().contains("rejected this API key"), "got: {err}");
        assert!(err.to_string().contains("Invalid API key"), "got: {err}");
    }

    #[test]
    fn verify_api_key_succeeds_on_a_real_200_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let body = r#"{"choices":[{"message":{"content":"pong"}}]}"#;
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let result = verify_api_key(&format!("http://127.0.0.1:{port}"), "openrouter/auto", "sk-real", "curl");
        handle.join().unwrap();
        assert!(result.is_ok(), "got: {result:?}");
    }
}
