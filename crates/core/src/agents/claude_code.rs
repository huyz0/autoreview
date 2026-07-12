use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use autoreview_schema::AgentFinding;

use super::contract::parse_agent_output;

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
    }
}

pub struct InvokeRequest {
    pub prompt: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    pub max_turns: u32,
    pub model: String,
    pub cwd: PathBuf,
}

pub struct InvokeResult {
    pub final_text: String,
    pub usage: Usage,
    pub wall_ms: u64,
}

/// Parses `claude -p --output-format stream-json` output: one JSON object per
/// line. NOTE: the exact event shape (event `type` values, where the final
/// text and usage numbers live) is a moving target across Claude Code
/// versions and hasn't been verified against a live invocation in this
/// environment — this parser is deliberately defensive rather than strict:
/// it scans every line for a `usage` object (summing across events, so an
/// undercounted single "source of truth" event still doesn't silently lose
/// tokens) and keeps the last non-empty text it can find from either an
/// assistant message's content or a terminal `result` field. Re-validate
/// this against `claude --version`'s actual output before trusting the cost
/// accounting in production — this is exactly the risk the plan calls out
/// ("Claude Code headless flags move — isolate assumptions in claude_code.rs").
pub fn parse_stream_json(stdout: &str) -> (String, Usage) {
    let mut usage = Usage::default();
    let mut last_text = String::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if let Some(usage_obj) = value.get("usage") {
            let input = usage_obj.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = usage_obj.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            usage.add(&Usage { input_tokens: input, output_tokens: output });
        }

        if let Some(result) = value.get("result").and_then(|v| v.as_str()) {
            if !result.is_empty() {
                last_text = result.to_string();
            }
        } else if let Some(message) = value.get("message") {
            if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
                for block in content {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            last_text = text.to_string();
                        }
                    }
                }
            }
        }
    }

    (last_text, usage)
}

/// Abstraction point for the agent backend, so specialist orchestration
/// (repair-turn retry, fail-soft on two failures) can be unit-tested against
/// a fake backend without spawning a real process.
pub trait AgentBackend {
    fn invoke(&self, req: &InvokeRequest) -> anyhow::Result<InvokeResult>;
}

pub struct ClaudeCodeBackend {
    pub binary: String,
}

impl Default for ClaudeCodeBackend {
    fn default() -> Self {
        Self { binary: "claude".to_string() }
    }
}

impl AgentBackend for ClaudeCodeBackend {
    fn invoke(&self, req: &InvokeRequest) -> anyhow::Result<InvokeResult> {
        let start = Instant::now();
        let output = Command::new(&self.binary)
            .arg("-p")
            .arg(&req.prompt)
            .arg("--append-system-prompt")
            .arg(&req.system_prompt)
            .arg("--allowedTools")
            .arg(req.allowed_tools.join(","))
            .arg("--max-turns")
            .arg(req.max_turns.to_string())
            .arg("--model")
            .arg(&req.model)
            .arg("--output-format")
            .arg("stream-json")
            .current_dir(&req.cwd)
            .output()?;

        let wall_ms = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            anyhow::bail!("claude exited with error: {}", String::from_utf8_lossy(&output.stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let (final_text, usage) = parse_stream_json(&stdout);
        Ok(InvokeResult { final_text, usage, wall_ms })
    }
}

#[derive(Debug, Clone)]
pub enum SpecialistStatus {
    Ok,
    Failed { reason: String },
}

pub struct SpecialistResult {
    pub aspect: String,
    pub findings: Vec<AgentFinding>,
    pub status: SpecialistStatus,
    pub usage: Usage,
    pub wall_ms: u64,
}

/// Runs one specialist to completion: invoke, validate against the output
/// contract, and on failure send exactly one repair turn (re-prompting with
/// the validation error and the agent's own prior output) before giving up
/// and marking the specialist failed. A failed specialist never aborts the
/// overall review — the caller folds an empty finding list in and moves on.
pub fn run_specialist(backend: &dyn AgentBackend, aspect: &str, base_request: InvokeRequest, task_prompt: &str) -> SpecialistResult {
    let mut usage_total = Usage::default();
    let mut wall_ms_total = 0u64;

    let first_request = InvokeRequest { prompt: task_prompt.to_string(), ..clone_request(&base_request) };

    let first = match backend.invoke(&first_request) {
        Ok(result) => result,
        Err(err) => {
            return SpecialistResult { aspect: aspect.to_string(), findings: vec![], status: SpecialistStatus::Failed { reason: err.to_string() }, usage: usage_total, wall_ms: wall_ms_total };
        }
    };
    usage_total.add(&first.usage);
    wall_ms_total += first.wall_ms;

    match parse_agent_output(&first.final_text) {
        Ok(output) => SpecialistResult { aspect: aspect.to_string(), findings: output.findings, status: SpecialistStatus::Ok, usage: usage_total, wall_ms: wall_ms_total },
        Err(first_err) => {
            let repair_prompt = format!(
                "Your previous response failed output-contract validation: {first_err}\n\nYour previous response was:\n{}\n\nRe-emit ONLY the corrected fenced ```json block now, matching the findings-json-v1 contract exactly.",
                truncate(&first.final_text, 2000)
            );
            let repair_request = InvokeRequest { prompt: repair_prompt, ..clone_request(&base_request) };

            match backend.invoke(&repair_request) {
                Ok(repair) => {
                    usage_total.add(&repair.usage);
                    wall_ms_total += repair.wall_ms;
                    match parse_agent_output(&repair.final_text) {
                        Ok(output) => SpecialistResult { aspect: aspect.to_string(), findings: output.findings, status: SpecialistStatus::Ok, usage: usage_total, wall_ms: wall_ms_total },
                        Err(second_err) => SpecialistResult {
                            aspect: aspect.to_string(),
                            findings: vec![],
                            status: SpecialistStatus::Failed { reason: format!("failed contract validation twice: first={first_err}, after-repair={second_err}") },
                            usage: usage_total,
                            wall_ms: wall_ms_total,
                        },
                    }
                }
                Err(repair_invoke_err) => SpecialistResult {
                    aspect: aspect.to_string(),
                    findings: vec![],
                    status: SpecialistStatus::Failed { reason: format!("first attempt failed contract ({first_err}), repair turn failed to invoke: {repair_invoke_err}") },
                    usage: usage_total,
                    wall_ms: wall_ms_total,
                },
            }
        }
    }
}

fn clone_request(req: &InvokeRequest) -> InvokeRequest {
    InvokeRequest {
        prompt: req.prompt.clone(),
        system_prompt: req.system_prompt.clone(),
        allowed_tools: req.allowed_tools.clone(),
        max_turns: req.max_turns,
        model: req.model.clone(),
        cwd: req.cwd.clone(),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}... [truncated]", s.chars().take(max_chars).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn parse_stream_json_accumulates_usage_across_events() {
        let stdout = "\
{\"type\":\"system\"}
{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"looking...\"}]},\"usage\":{\"input_tokens\":100,\"output_tokens\":20}}
{\"type\":\"result\",\"result\":\"```json\\n{\\\"findings\\\": []}\\n```\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}
";
        let (text, usage) = parse_stream_json(stdout);
        assert_eq!(usage.input_tokens, 110);
        assert_eq!(usage.output_tokens, 25);
        assert!(text.contains("findings"));
    }

    #[test]
    fn parse_stream_json_ignores_malformed_lines_without_failing() {
        let stdout = "not json\n{\"type\":\"result\",\"result\":\"ok\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n";
        let (text, usage) = parse_stream_json(stdout);
        assert_eq!(text, "ok");
        assert_eq!(usage.input_tokens, 1);
    }

    fn make_request() -> InvokeRequest {
        InvokeRequest {
            prompt: "review this".to_string(),
            system_prompt: "you are a reviewer".to_string(),
            allowed_tools: vec!["Read".to_string()],
            max_turns: 10,
            model: "sonnet".to_string(),
            cwd: PathBuf::from("/repo"),
        }
    }

    struct ScriptedBackend {
        responses: RefCell<Vec<anyhow::Result<String>>>,
    }

    impl AgentBackend for ScriptedBackend {
        fn invoke(&self, _req: &InvokeRequest) -> anyhow::Result<InvokeResult> {
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                anyhow::bail!("no more scripted responses");
            }
            let next = responses.remove(0);
            next.map(|text| InvokeResult { final_text: text, usage: Usage::default(), wall_ms: 1 })
        }
    }

    const VALID_FINDING: &str = "```json\n{\"findings\": [{\"source\": {\"kind\": \"agent\", \"tool\": \"claude-code\", \"aspect\": \"security\"}, \"category\": \"security\", \"severity\": \"high\", \"confidence\": 0.8, \"title\": \"t\", \"message\": \"m\", \"location\": {\"path\": \"a.ts\", \"range\": {\"startLine\": 1}, \"snippet\": \"x\", \"side\": \"new\"}}]}\n```";

    #[test]
    fn succeeds_on_first_valid_response_without_a_repair_turn() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok(VALID_FINDING.to_string())]) };
        let result = run_specialist(&backend, "security", make_request(), "review this diff");
        assert!(matches!(result.status, SpecialistStatus::Ok));
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn recovers_via_one_repair_turn_after_an_invalid_first_response() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok("no json here, oops".to_string()), Ok(VALID_FINDING.to_string())]) };
        let result = run_specialist(&backend, "security", make_request(), "review this diff");
        assert!(matches!(result.status, SpecialistStatus::Ok));
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn marks_failed_after_two_contract_failures_without_aborting() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![Ok("still no json".to_string()), Ok("still no json after repair".to_string())]) };
        let result = run_specialist(&backend, "security", make_request(), "review this diff");
        assert!(matches!(result.status, SpecialistStatus::Failed { .. }));
        assert_eq!(result.findings.len(), 0);
    }

    #[test]
    fn marks_failed_gracefully_when_the_process_itself_fails_to_invoke() {
        let backend = ScriptedBackend { responses: RefCell::new(vec![]) };
        let result = run_specialist(&backend, "security", make_request(), "review this diff");
        assert!(matches!(result.status, SpecialistStatus::Failed { .. }));
    }
}
