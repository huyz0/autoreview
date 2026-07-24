//! Agent-backend dispatch (`AgentBackendKind` -> its label, availability
//! check, and constructed `AgentBackend`), shared by every CLI command that
//! runs specialists/skills against a configurable backend. Previously
//! duplicated verbatim across `diff.rs`/`rules.rs`/`skills.rs`/
//! `skills_bench.rs` — adding a fourth backend meant editing all four; now
//! it's one match per function, here.

use std::process::Command;

use autoreview_core::{AgentBackend, ClaudeCodeBackend, CredentialStore, LocalLlmBackend, OpenAiCompatibleBackend, PiBackend, OPENAI_COMPAT_ACCOUNT, OPENAI_COMPAT_SERVICE};
use autoreview_schema::{AgentBackendKind, AutoreviewConfig};

pub fn backend_label(kind: AgentBackendKind) -> &'static str {
    match kind {
        AgentBackendKind::ClaudeCode => "claude",
        AgentBackendKind::Pi => "pi",
        AgentBackendKind::LocalLlm => "local-llm",
        AgentBackendKind::OpenAiCompatible => "openai-compatible",
    }
}

/// The stored API key for the OpenAI-compatible backend, if any — `None`
/// covers both "never logged in" and a real store error (e.g. a fallback
/// file that can't be read); either way there's no key to use, so both
/// collapse to the same `None` here rather than this helper propagating
/// an error `backend_available`'s plain-bool contract has no room for.
fn openai_compatible_api_key() -> Option<String> {
    CredentialStore::open_default().load(OPENAI_COMPAT_SERVICE, OPENAI_COMPAT_ACCOUNT).ok().flatten()
}

pub fn backend_available(kind: AgentBackendKind, config: &AutoreviewConfig) -> bool {
    match kind {
        AgentBackendKind::ClaudeCode => Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::Pi => Command::new("pi").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::LocalLlm => autoreview_core::local_llm_available(&config.agents.local_llm.base_url, "curl"),
        AgentBackendKind::OpenAiCompatible => match openai_compatible_api_key() {
            Some(api_key) => autoreview_core::openai_compatible_available(&config.agents.open_ai_compatible.base_url, &api_key, "curl"),
            None => false,
        },
    }
}

pub fn build_backend(kind: AgentBackendKind, config: &AutoreviewConfig) -> Box<dyn AgentBackend + Sync> {
    match kind {
        AgentBackendKind::ClaudeCode => Box::new(ClaudeCodeBackend::default()),
        AgentBackendKind::Pi => Box::new(PiBackend { binary: "pi".to_string(), provider: config.agents.pi_provider.clone() }),
        AgentBackendKind::LocalLlm => Box::new(LocalLlmBackend { base_url: config.agents.local_llm.base_url.clone(), curl_binary: "curl".to_string() }),
        AgentBackendKind::OpenAiCompatible => Box::new(OpenAiCompatibleBackend {
            base_url: config.agents.open_ai_compatible.base_url.clone(),
            api_key: openai_compatible_api_key().unwrap_or_default(),
            curl_binary: "curl".to_string(),
        }),
    }
}
