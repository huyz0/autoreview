//! Drafts a `.autoreview/spec.md` scaffold from a diff via the configured
//! agent backend — `autoreview spec draft`'s core logic. A human still
//! reviews and edits the result before it does anything (nothing here
//! writes the file itself, and `run_spec_verify` only ever reads whatever
//! ends up on disk); this only saves the blank-page problem of writing an
//! acceptance-criteria spec from scratch, in the exact format `parse::
//! parse_spec` expects.

use crate::agents::claude_code::{AgentBackend, InvokeRequest, Usage};
use crate::agents::contract::extract_last_fenced_block;

#[derive(Debug)]
pub struct DraftedSpec {
    pub markdown: String,
    pub usage: Usage,
}

fn build_draft_prompt(diff_text: &str) -> String {
    format!(
        "You are drafting an acceptance-criteria spec for a code change, in the exact format below — this becomes `.autoreview/spec.md`, later checked against the diff by an LLM judge, so each criterion must be a specific, independently-verifiable claim about what the diff actually does, not a vague restatement of \"the code should work\" or \"the code should be correct.\"\n\n\
        Format (match exactly — a `# ` title, a `## Intent` section with 1-2 sentences on why this change exists, and a `## Acceptance Criteria` section with 3-6 bullet points):\n\n\
        # <short title>\n\n## Intent\n\n<1-2 sentences>\n\n## Acceptance Criteria\n\n- <specific, checkable claim>\n- <specific, checkable claim>\n\n\
        The diff:\n```diff\n{diff_text}\n```\n\n\
        Respond with ONLY a fenced ```markdown block containing the spec, nothing else — no preamble, no explanation outside the block."
    )
}

/// Invokes `backend` once with the full diff text and parses its response
/// as a fenced markdown block. Doesn't validate the drafted content against
/// `parse::parse_spec` — a draft that doesn't quite match the expected
/// shape is still useful raw material for a human to fix up, so this
/// deliberately doesn't reject it the way `run_spec_verify`'s own contract
/// checking does for its JSON response.
pub fn draft_spec(backend: &dyn AgentBackend, model: &str, diff_text: &str, cwd: &std::path::Path) -> anyhow::Result<DraftedSpec> {
    let request = InvokeRequest {
        prompt: build_draft_prompt(diff_text),
        system_prompt: "You write clear, specific, verifiable acceptance criteria for code changes — never vague restatements of \"works correctly.\"".to_string(),
        allowed_tools: vec![],
        max_turns: 1,
        model: model.to_string(),
        cwd: cwd.to_path_buf(),
    };
    let invoked = backend.invoke(&request)?;
    let markdown = extract_last_fenced_block(&invoked.final_text).ok_or_else(|| anyhow::anyhow!("agent response had no fenced block"))?.trim().to_string();
    Ok(DraftedSpec { markdown, usage: invoked.usage })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct ScriptedBackend {
        response: RefCell<Option<anyhow::Result<String>>>,
    }

    impl AgentBackend for ScriptedBackend {
        fn invoke(&self, _req: &InvokeRequest) -> anyhow::Result<crate::agents::claude_code::InvokeResult> {
            let response = self.response.borrow_mut().take().expect("scripted response exhausted");
            let final_text = response?;
            Ok(crate::agents::claude_code::InvokeResult { final_text, usage: Usage::default(), wall_ms: 1 })
        }
    }

    #[test]
    fn extracts_the_drafted_markdown_from_a_fenced_block() {
        let backend = ScriptedBackend {
            response: RefCell::new(Some(Ok("Sure, here's a draft:\n```markdown\n# Add rate limiting\n\n## Intent\n\nCap per-user request rate.\n\n## Acceptance Criteria\n\n- Requests over the limit return 429\n```\n".to_string()))),
        };
        let drafted = draft_spec(&backend, "haiku", "diff --git a/x b/x\n", std::path::Path::new("/repo")).unwrap();
        assert!(drafted.markdown.starts_with("# Add rate limiting"), "got: {}", drafted.markdown);
        assert!(drafted.markdown.contains("## Acceptance Criteria"), "got: {}", drafted.markdown);
    }

    #[test]
    fn errors_clearly_when_the_response_has_no_fenced_block() {
        let backend = ScriptedBackend { response: RefCell::new(Some(Ok("I couldn't draft one.".to_string()))) };
        let err = draft_spec(&backend, "haiku", "diff --git a/x b/x\n", std::path::Path::new("/repo")).unwrap_err();
        assert!(err.to_string().contains("fenced block"), "got: {err}");
    }

    #[test]
    fn propagates_an_invocation_failure() {
        let backend = ScriptedBackend { response: RefCell::new(Some(Err(anyhow::anyhow!("agent process failed to launch")))) };
        let err = draft_spec(&backend, "haiku", "diff --git a/x b/x\n", std::path::Path::new("/repo")).unwrap_err();
        assert!(err.to_string().contains("failed to launch"), "got: {err}");
    }
}
