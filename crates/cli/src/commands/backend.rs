//! Agent-backend dispatch (`AgentBackendKind` -> its label, availability
//! check, and constructed `AgentBackend`), shared by every CLI command that
//! runs specialists/skills against a configurable backend. Previously
//! duplicated verbatim across `diff.rs`/`rules.rs`/`skills.rs`/
//! `skills_bench.rs` — adding a fourth backend meant editing all four; now
//! it's one match per function, here.

use std::process::Command;

use autoreview_core::{AgentBackend, ClaudeCodeBackend, LocalLlmBackend, PiBackend};
use autoreview_schema::{AgentBackendKind, AutoreviewConfig};

pub fn backend_label(kind: AgentBackendKind) -> &'static str {
    match kind {
        AgentBackendKind::ClaudeCode => "claude",
        AgentBackendKind::Pi => "pi",
        AgentBackendKind::LocalLlm => "local-llm",
    }
}

pub fn backend_available(kind: AgentBackendKind, config: &AutoreviewConfig) -> bool {
    match kind {
        AgentBackendKind::ClaudeCode => Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::Pi => Command::new("pi").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::LocalLlm => autoreview_core::local_llm_available(&config.agents.local_llm.base_url, "curl"),
    }
}

pub fn build_backend(kind: AgentBackendKind, config: &AutoreviewConfig) -> Box<dyn AgentBackend + Sync> {
    match kind {
        AgentBackendKind::ClaudeCode => Box::new(ClaudeCodeBackend::default()),
        AgentBackendKind::Pi => Box::new(PiBackend { binary: "pi".to_string(), provider: config.agents.pi_provider.clone() }),
        AgentBackendKind::LocalLlm => Box::new(LocalLlmBackend { base_url: config.agents.local_llm.base_url.clone(), curl_binary: "curl".to_string() }),
    }
}
