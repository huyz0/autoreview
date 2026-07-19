//! `pi` (pi.dev's coding-agent CLI) backend — the second `AgentBackend`
//! implementation, proving the abstraction the plan's M3 milestone calls
//! for ("raw Anthropic API backend... proves the abstraction") a milestone
//! early, since the trait was already shaped for exactly this.
//!
//! The invocation shape and JSON event types below are taken directly from
//! pi's own shipped TypeScript type definitions and `docs/json.md`
//! (`AssistantMessage`/`Usage` in `@earendil-works/pi-ai`'s `types.d.ts`,
//! and the `agent_end`/`message_end` event shapes in `docs/json.md`), not
//! guessed — the same rigor the Claude Code adapter's live verification
//! pass used, applied here via source inspection since a live invocation
//! needs a provider credential this environment doesn't have configured.
//! Flagged explicitly in code and to the user: this parser has NOT been run
//! against a live `pi` process yet.

use std::process::Command;
use std::time::Instant;

use serde::Deserialize;

use super::claude_code::{AgentBackend, InvokeRequest, InvokeResult, Usage};

/// pi has no fine-grained per-command Bash allowlist like Claude Code's
/// `Bash(cmd:*)` — its `bash` tool is monolithic. Any requested Bash command
/// capability is mapped down to enabling the whole `bash` tool, a coarser
/// grant than what was actually requested. This is a real, intentional
/// fidelity loss, not an oversight — documented here so it isn't rediscovered
/// as a mystery later.
fn map_allowed_tools(claude_style_tools: &[String]) -> String {
    let mut pi_tools = Vec::new();
    for tool in claude_style_tools {
        match tool.as_str() {
            "Read" => pi_tools.push("read"),
            "Grep" => pi_tools.push("grep"),
            "Glob" => pi_tools.push("find"),
            t if t.starts_with("Bash(") => pi_tools.push("bash"),
            _ => {}
        }
    }
    pi_tools.sort();
    pi_tools.dedup();
    pi_tools.join(",")
}

/// `messages` in the `agent_end` event is a heterogeneous union
/// (`UserMessage.content` is a bare string; `AssistantMessage.content` is an
/// array of typed blocks; `ToolResultMessage` has yet another shape) —
/// deserializing straight into an assistant-shaped struct fails the whole
/// array the moment a non-assistant message shows up, so `content`/`usage`
/// are pulled generically and interpreted per-role instead.
#[derive(Debug, Deserialize)]
struct PiMessage {
    role: String,
    #[serde(default)]
    content: serde_json::Value,
    #[serde(default)]
    usage: Option<PiUsage>,
}

#[derive(Debug, Deserialize)]
struct PiUsage {
    input: u64,
    output: u64,
    cost: Option<PiCost>,
}

#[derive(Debug, Deserialize)]
struct PiCost {
    total: f64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum PiEvent {
    #[serde(rename = "agent_end")]
    AgentEnd { messages: Vec<PiMessage> },
    #[serde(other)]
    Other,
}

/// Parses `pi --mode json` output (one JSON object per line): scans for the
/// terminal `agent_end` event, which carries the full message list per
/// `docs/json.md`, and takes the last assistant message's text content plus
/// its usage/cost — the same "last assistant message is the answer" pattern
/// as the Claude Code adapter, adapted to pi's documented event shape rather
/// than assumed to match it.
pub fn parse_pi_json_output(stdout: &str) -> (String, Usage) {
    let mut usage = Usage::default();
    let mut final_text = String::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<PiEvent>(line) else { continue };
        let PiEvent::AgentEnd { messages } = event else { continue };

        for message in messages.iter().rev() {
            if message.role != "assistant" {
                continue;
            }
            let text: String = message
                .content
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"))
                .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                final_text = text;
            }
            if let Some(u) = &message.usage {
                usage.input_tokens += u.input;
                usage.output_tokens += u.output;
                usage.usd = match (usage.usd, u.cost.as_ref().map(|c| c.total)) {
                    (Some(a), Some(b)) => Some(a + b),
                    (Some(a), None) => Some(a),
                    (None, b) => b,
                };
            }
            break; // last assistant message only, per the doc's own example
        }
    }

    (final_text, usage)
}

pub struct PiBackend {
    pub binary: String,
    /// Optional `provider/model` override; when `None`, `InvokeRequest.model`
    /// is passed through as-is to `--model` (pi resolves bare model ids
    /// against whatever provider is configured/logged in).
    pub provider: Option<String>,
}

impl Default for PiBackend {
    fn default() -> Self {
        Self { binary: "pi".to_string(), provider: None }
    }
}

impl AgentBackend for PiBackend {
    fn invoke(&self, req: &InvokeRequest) -> anyhow::Result<InvokeResult> {
        let start = Instant::now();
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--print")
            .arg("--mode")
            .arg("json")
            .arg("--no-session")
            .arg("--system-prompt")
            .arg(&req.system_prompt)
            .arg("--model")
            .arg(&req.model);

        let tools = map_allowed_tools(&req.allowed_tools);
        if tools.is_empty() {
            cmd.arg("--no-tools");
        } else {
            cmd.arg("--tools").arg(&tools);
        }
        if let Some(provider) = &self.provider {
            cmd.arg("--provider").arg(provider);
        }
        // pi has no CLI flag for a per-invocation turn cap (verified against
        // `pi --help`) — `req.max_turns` is unenforceable on this backend,
        // unlike Claude Code's `--max-turns`. A real, intentional gap:
        // budget accounting still records whatever pi actually spends. Warn
        // once per process rather than staying silent, so a configured
        // turn budget doesn't look enforced when it isn't.
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            eprintln!("warning: pi backend has no per-invocation max-turns flag — req.max_turns ({}) is not enforced", req.max_turns);
        });

        cmd.arg(&req.prompt).current_dir(&req.cwd);
        let output = cmd.output()?;
        let wall_ms = start.elapsed().as_millis() as u64;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("pi exited with error: {}", if stderr.trim().is_empty() { "(no stderr captured)" } else { stderr.trim() });
        }

        let (final_text, usage) = parse_pi_json_output(&stdout);
        Ok(InvokeResult { final_text, usage, wall_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_claude_style_tool_names_to_pi_tool_names() {
        assert_eq!(map_allowed_tools(&["Read".to_string(), "Grep".to_string(), "Glob".to_string()]), "find,grep,read");
    }

    #[test]
    fn collapses_any_bash_command_capability_to_the_single_bash_tool() {
        assert_eq!(map_allowed_tools(&["Bash(go build:*)".to_string(), "Bash(go test:*)".to_string()]), "bash");
    }

    #[test]
    fn empty_tool_list_maps_to_empty_string() {
        assert_eq!(map_allowed_tools(&[]), "");
    }

    #[test]
    fn parses_the_documented_agent_end_shape_and_takes_the_last_assistant_message() {
        // Shape taken from pi's shipped types.d.ts (AssistantMessage/Usage)
        // and docs/json.md's agent_end example — not a live capture.
        let stdout = r#"{"type":"session","version":3,"id":"x","timestamp":"t","cwd":"/repo"}
{"type":"agent_start"}
{"type":"agent_end","messages":[{"role":"user","content":"review this"},{"role":"assistant","content":[{"type":"thinking","thinking":"let me look"},{"type":"text","text":"```json\n{\"findings\": []}\n```"}],"api":"chat","provider":"anthropic","model":"claude-sonnet-4","usage":{"input":100,"output":50,"cacheRead":0,"cacheWrite":0,"totalTokens":150,"cost":{"input":0.001,"output":0.002,"cacheRead":0.0,"cacheWrite":0.0,"total":0.003}},"stopReason":"stop","timestamp":1}]}
"#;
        let (text, usage) = parse_pi_json_output(stdout);
        assert!(text.contains("findings"));
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.usd, Some(0.003));
    }

    #[test]
    fn ignores_non_assistant_messages_in_agent_end() {
        let stdout = r#"{"type":"agent_end","messages":[{"role":"user","content":"hi"},{"role":"toolResult","content":[],"isError":false,"toolCallId":"x","toolName":"read","timestamp":1}]}"#;
        let (text, usage) = parse_pi_json_output(stdout);
        assert!(text.is_empty());
        assert_eq!(usage.input_tokens, 0);
    }

    #[test]
    fn ignores_malformed_lines_without_failing() {
        let stdout = "not json\n{\"type\":\"agent_start\"}\n";
        let (text, usage) = parse_pi_json_output(stdout);
        assert!(text.is_empty());
        assert_eq!(usage.input_tokens, 0);
    }

    #[test]
    fn thinking_only_content_produces_no_final_text() {
        let stdout = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"}],"api":"chat","provider":"anthropic","model":"m","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2},"stopReason":"stop","timestamp":1}]}"#;
        let (text, _usage) = parse_pi_json_output(stdout);
        assert!(text.is_empty());
    }
}
