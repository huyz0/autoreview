//! Credential storage and login flows for the external hosts this project
//! can talk to directly: GitHub (`github_device_flow`, RFC 8628 OAuth
//! device flow), Bitbucket Cloud (`bitbucket_token`, HTTP Basic auth with
//! an Atlassian API token), and a generic OpenAI-compatible hosted
//! backend (`agents::openai_compatible` — a plain bearer API key, no
//! provider-specific verification since none is standardized across the
//! providers this backend targets). `credential_store` is the shared
//! piece all three build on — see its own module doc for the storage-order
//! design (env var, then OS keyring, then a locked-down fallback file).
//!
//! Kept in `core` rather than `cli`, consistent with every other
//! shell-out/OS-integration piece in this codebase (`agents::local_llm`,
//! `agents::embedding`, `agents::claude_code`) — the CLI layer
//! (`crates/cli/src/commands/auth.rs`) is a thin wrapper handling
//! terminal I/O (prompts, `--email`/`--token` flags) around these.

pub mod bitbucket_token;
pub mod credential_store;
pub mod github_device_flow;

/// The `keyring`/fallback-file "service" name for each provider this
/// project supports — shared here so `credential_store`'s callers never
/// have to hand-spell them, which would risk one call site quietly
/// drifting from another (e.g. a typo'd `"autoreview-github "` that
/// silently never matches what `auth login` stored).
pub const GITHUB_SERVICE: &str = "autoreview-github";
pub const BITBUCKET_SERVICE: &str = "autoreview-bitbucket";
pub const OPENAI_COMPAT_SERVICE: &str = "autoreview-openai-compat";
/// The fixed account name under which the GitHub OAuth token is stored —
/// there's exactly one GitHub identity this tool authenticates as, unlike
/// Bitbucket where the account name is the user's own email address (see
/// `bitbucket_token`'s docs).
pub const GITHUB_ACCOUNT: &str = "oauth-token";
/// Same reasoning as `GITHUB_ACCOUNT`: `agents.openAiCompatible` is a
/// single config slot (one active provider/base_url at a time), so one
/// fixed account name is enough — there's no per-provider identity to key
/// on the way Bitbucket's account-is-an-email design needs.
pub const OPENAI_COMPAT_ACCOUNT: &str = "api-key";
