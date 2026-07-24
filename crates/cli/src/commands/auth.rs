//! `autoreview auth status`/`login`/`logout` — manages the credentials
//! `rule_factory::mine_from_bitbucket_comments` (and, for GitHub, a future
//! network-backed source beyond the existing `gh`-shelling-out
//! `mine_from_comments`) need. Thin terminal-I/O wrapper around
//! `autoreview_core::auth`'s `CredentialStore` and the two provider-
//! specific login flows — see that module's own docs for the storage
//! design and why this project stores credentials itself at all instead
//! of only ever delegating to an already-authenticated CLI.

use autoreview_core::{CredentialStore, BITBUCKET_SERVICE, GITHUB_ACCOUNT, GITHUB_SERVICE};

struct StatusLine {
    provider: &'static str,
    ok: bool,
    detail: String,
}

fn print_status_line(line: &StatusLine) {
    let icon = if line.ok { "✓" } else { "✗" };
    println!("  {icon} {:<10} {}", line.provider, line.detail);
}

fn github_status(store: &CredentialStore) -> anyhow::Result<StatusLine> {
    let logged_in = store.load(GITHUB_SERVICE, GITHUB_ACCOUNT)?.is_some();
    let detail = if logged_in { "logged in".to_string() } else { "not logged in — run `autoreview auth login github`".to_string() };
    Ok(StatusLine { provider: "github", ok: logged_in, detail })
}

fn bitbucket_status(store: &CredentialStore) -> anyhow::Result<StatusLine> {
    let account = store.recall_account(BITBUCKET_SERVICE);
    let logged_in = match &account {
        Some(email) => store.load(BITBUCKET_SERVICE, email)?.is_some(),
        None => false,
    };
    let detail = match (&account, logged_in) {
        (Some(email), true) => format!("logged in as {email}"),
        _ => "not logged in — run `autoreview auth login bitbucket`".to_string(),
    };
    Ok(StatusLine { provider: "bitbucket", ok: logged_in, detail })
}

/// Read-only by default — checks only whether a credential is present in
/// the local `CredentialStore`, no network call. Never errors on "not
/// logged in": that's a normal, expected state this command exists to
/// report, not a failure of the command itself.
pub fn run_auth_status() -> anyhow::Result<()> {
    let store = CredentialStore::open_default();
    println!("autoreview auth status\n");
    print_status_line(&github_status(&store)?);
    print_status_line(&bitbucket_status(&store)?);
    Ok(())
}
