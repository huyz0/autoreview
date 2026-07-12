use autoreview_schema::AgentOutput;
use std::fmt;

#[derive(Debug)]
pub enum ContractError {
    /// The final message had no fenced code block at all — the most common
    /// way a specialist agent fails to honor the output contract.
    NoFencedBlock,
    /// A fenced block was found but its contents didn't deserialize into
    /// `AgentOutput`. Carries the parse error so it can be relayed verbatim
    /// in the repair-turn prompt ("your output failed validation: <errors>").
    InvalidJson(serde_json::Error),
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractError::NoFencedBlock => write!(f, "no fenced ```json code block found in the final message"),
            ContractError::InvalidJson(err) => write!(f, "fenced block did not match the findings-json-v1 contract: {err}"),
        }
    }
}

impl std::error::Error for ContractError {}

/// Extracts the *last* fenced code block in `text` — a specialist may show
/// prose or example snippets before its final answer, so taking the last
/// block (rather than the first) is what makes the contract robust to that.
/// Accepts both ```json and bare ``` fences.
pub fn extract_last_fenced_block(text: &str) -> Option<&str> {
    let mut search_from = 0usize;
    let mut last_block: Option<&str> = None;

    loop {
        let Some(open_rel) = text[search_from..].find("```") else {
            // No further fence — return whatever we already found, don't
            // let a failed search on a later iteration discard it.
            return last_block;
        };
        let open_idx = search_from + open_rel;
        let after_open = open_idx + 3;

        // Skip an optional language tag (e.g. "json") up to the newline.
        let line_end_rel = text[after_open..].find('\n');
        let content_start = match line_end_rel {
            Some(rel) => after_open + rel + 1,
            None => return last_block, // opening fence with no newline: malformed, stop here
        };

        let close_rel = match text[content_start..].find("```") {
            Some(rel) => rel,
            None => return last_block, // unterminated fence: ignore this one
        };
        let content_end = content_start + close_rel;

        last_block = Some(text[content_start..content_end].trim());
        search_from = content_end + 3;

        if search_from >= text.len() {
            break;
        }
    }

    last_block
}

/// Parses an agent's final message against the `findings-json-v1` output
/// contract: extract the last fenced block, deserialize as `AgentOutput`.
pub fn parse_agent_output(text: &str) -> Result<AgentOutput, ContractError> {
    let block = extract_last_fenced_block(text).ok_or(ContractError::NoFencedBlock)?;
    serde_json::from_str(block).map_err(ContractError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_block() {
        let text = r#"I reviewed the diff and found one issue.

```json
{"findings": [{"source": {"kind": "agent", "tool": "claude-code", "aspect": "security"}, "category": "security", "severity": "high", "confidence": 0.8, "title": "t", "message": "m", "location": {"path": "a.ts", "range": {"startLine": 1}, "snippet": "x", "side": "new"}}]}
```
"#;
        let output = parse_agent_output(text).unwrap();
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].category, "security");
    }

    #[test]
    fn parses_an_empty_findings_list() {
        let text = "No issues found.\n\n```json\n{\"findings\": []}\n```\n";
        let output = parse_agent_output(text).unwrap();
        assert_eq!(output.findings.len(), 0);
    }

    #[test]
    fn takes_the_last_block_when_multiple_are_present() {
        let text = r#"Here's an example of the format:

```json
{"findings": []}
```

But actually I found something:

```json
{"findings": [{"source": {"kind": "agent", "tool": "claude-code", "aspect": "security"}, "category": "security", "severity": "low", "confidence": 0.5, "title": "t", "message": "m", "location": {"path": "a.ts", "range": {"startLine": 1}, "snippet": "x", "side": "new"}}]}
```
"#;
        let output = parse_agent_output(text).unwrap();
        assert_eq!(output.findings.len(), 1);
    }

    #[test]
    fn errors_clearly_when_no_fenced_block_exists() {
        let err = parse_agent_output("I looked at the diff and it seems fine, no JSON here.").unwrap_err();
        assert!(matches!(err, ContractError::NoFencedBlock));
    }

    #[test]
    fn errors_with_the_parse_failure_on_malformed_json() {
        let text = "```json\n{\"findings\": [}\n```";
        let err = parse_agent_output(text).unwrap_err();
        assert!(matches!(err, ContractError::InvalidJson(_)));
    }

    #[test]
    fn accepts_a_bare_fence_without_a_json_language_tag() {
        let text = "```\n{\"findings\": []}\n```";
        let output = parse_agent_output(text).unwrap();
        assert_eq!(output.findings.len(), 0);
    }
}
