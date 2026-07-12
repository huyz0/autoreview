//! Local-LLM backend — targets an OpenAI-compatible `/v1/chat/completions`
//! endpoint, the shape llama.cpp's `llama-server` exposes (also LM Studio,
//! vLLM, Ollama's compat layer — genuinely the industry-standard contract,
//! not a guess the way a brand-new tool's CLI would be). Uses `curl` rather
//! than pulling in an HTTP client crate, matching this project's existing
//! pattern of shelling out to external processes (git, ast-grep,
//! golangci-lint, claude, pi) instead of adding an async runtime.
//!
//! No tool access: local models served this way have inconsistent
//! tool-calling support, so this backend sends a single-shot chat completion
//! with everything (diff, context) already inlined into the prompt — a real,
//! intentional capability difference from the agentic backends, not an
//! oversight. `allowed_tools`/`max_turns` on the request are ignored.

use std::process::Command;
use std::time::Instant;

use serde::Deserialize;

use super::claude_code::{AgentBackend, InvokeRequest, InvokeResult, Usage};

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

/// Parses an OpenAI-compatible `/v1/chat/completions` JSON response body
/// (not a stream — `stream: false`) into `(text, usage)`. No `usd` field:
/// unlike the hosted backends, a local server has no dollar cost to report.
pub fn parse_chat_completion_response(body: &str) -> anyhow::Result<(String, Usage)> {
    let parsed: ChatCompletionResponse = serde_json::from_str(body)?;
    let text = parsed.choices.first().and_then(|c| c.message.content.clone()).unwrap_or_default();
    let usage = match parsed.usage {
        Some(u) => Usage { input_tokens: u.prompt_tokens, output_tokens: u.completion_tokens, usd: None },
        None => Usage::default(),
    };
    Ok((text, usage))
}

pub struct LocalLlmBackend {
    /// Base URL up to and including `/v1`, e.g. `http://localhost:8080/v1`
    /// (llama-server's default) — `/chat/completions` is appended.
    pub base_url: String,
    pub curl_binary: String,
}

impl Default for LocalLlmBackend {
    fn default() -> Self {
        Self { base_url: "http://localhost:8080/v1".to_string(), curl_binary: "curl".to_string() }
    }
}

impl AgentBackend for LocalLlmBackend {
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
        let output = Command::new(&self.curl_binary)
            .args(["-sS", "-X", "POST", &url, "-H", "Content-Type: application/json", "-d", "@-"])
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
            anyhow::bail!("local-llm request failed: {}", if stderr.trim().is_empty() { "(no stderr captured)" } else { stderr.trim() });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let (final_text, usage) = parse_chat_completion_response(&stdout)
            .map_err(|err| anyhow::anyhow!("local-llm response did not match the OpenAI chat-completions contract: {err}\nraw response: {}", super::claude_code::truncate(&stdout, 500)))?;
        Ok(InvokeResult { final_text, usage, wall_ms })
    }
}

/// Checks whether a local OpenAI-compatible server is reachable at
/// `base_url` — used the same way `claude --version` gates Stage 3.
///
/// Plain `curl` exits 0 for *any* completed HTTP response, including a 404
/// — verified the hard way: this returned `true` against an unrelated dev
/// server that happened to be listening on the default port and answered
/// with a 404. `--fail` makes curl itself exit non-zero on 4xx/5xx, so a
/// same-port-different-service false positive can't slip through again.
pub fn local_llm_available(base_url: &str, curl_binary: &str) -> bool {
    Command::new(curl_binary)
        .args(["-sS", "-f", "-o", "/dev/null", "--max-time", "2", &format!("{}/models", base_url.trim_end_matches('/'))])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_openai_chat_completion_response() {
        let body = r#"{"id":"x","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"```json\n{\"findings\": []}\n```"},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":30,"total_tokens":150}}"#;
        let (text, usage) = parse_chat_completion_response(body).unwrap();
        assert!(text.contains("findings"));
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.usd, None, "local server responses have no dollar cost");
    }

    #[test]
    fn defaults_usage_to_zero_when_the_response_omits_it() {
        let body = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        let (text, usage) = parse_chat_completion_response(body).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.input_tokens, 0);
    }

    #[test]
    fn errors_clearly_on_malformed_json() {
        assert!(parse_chat_completion_response("not json").is_err());
    }

    #[test]
    fn errors_clearly_when_choices_array_is_missing() {
        assert!(parse_chat_completion_response(r#"{"error": "model not found"}"#).is_err());
    }

    #[test]
    fn local_llm_available_is_false_when_curl_itself_is_missing() {
        assert!(!local_llm_available("http://localhost:8080/v1", "definitely-not-a-real-binary-xyz"));
    }

    /// Regression test for a real bug caught by hand: an unrelated dev
    /// server happened to be listening on the default llama.cpp port and
    /// answered every request with a 404, and `curl` without `--fail`
    /// treats that as a successful request (exit 0) — so
    /// `local_llm_available` reported `true` for a server that wasn't
    /// llama.cpp at all. Spins up a real TCP listener that always answers
    /// 404, exactly reproducing that shape.
    #[test]
    fn local_llm_available_is_false_against_a_server_that_only_returns_404() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let available = local_llm_available(&format!("http://127.0.0.1:{port}"), "curl");
        handle.join().unwrap();
        assert!(!available, "a 404-only server must not be reported as an available local-LLM backend");
    }
}
