//! `autoreview auth status`/`login`/`logout` — manages the credentials
//! `rule_factory::mine_from_bitbucket_comments` (and, for GitHub, a future
//! network-backed source beyond the existing `gh`-shelling-out
//! `mine_from_comments`) need. Thin terminal-I/O wrapper around
//! `autoreview_core::auth`'s `CredentialStore` and the two provider-
//! specific login flows — see that module's own docs for the storage
//! design and why this project stores credentials itself at all instead
//! of only ever delegating to an already-authenticated CLI.

use std::io::Write;
use std::path::Path;

use autoreview_core::{CredentialStore, StoredVia, BITBUCKET_SERVICE, GITHUB_ACCOUNT, GITHUB_SERVICE};

/// The OAuth scope requested for GitHub's device flow — `repo` (not the
/// narrower `public_repo`) since reading PR review comments on a private
/// repository, the realistic common case, needs it. See
/// `autoreview_core::auth::github_device_flow`'s own module doc for the
/// full tradeoff.
const GITHUB_OAUTH_SCOPE: &str = "repo";

/// Providers `auth login` currently knows how to log into — checked
/// explicitly rather than letting an unrecognized provider silently
/// no-op, matching this project's house style of naming exactly what
/// went wrong. Grows as more login flows land.
const KNOWN_LOGIN_PROVIDERS: &[&str] = &["bitbucket", "github"];

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

fn prompt_line(label: &str) -> anyhow::Result<String> {
    print!("{label}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn run_auth_login_bitbucket(email: Option<String>, token: Option<String>) -> anyhow::Result<()> {
    let email = match email {
        Some(e) if !e.trim().is_empty() => e.trim().to_string(),
        _ => prompt_line("Bitbucket account email: ")?,
    };
    if email.is_empty() {
        anyhow::bail!("no email provided");
    }

    let token = match token {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        // Not `.unwrap_or_default()` collapsed into the match above —
        // an empty --token flag value and "no flag at all" both fall
        // through to the same interactive prompt rather than silently
        // trying an empty string against the real API.
        _ => rpassword::prompt_password("Bitbucket API token (input hidden, create one at id.atlassian.com): ")?,
    };
    if token.is_empty() {
        anyhow::bail!("no API token provided");
    }

    println!("Verifying against the real Bitbucket API...");
    let user = autoreview_core::verify_bitbucket_token(&email, &token, "curl")?;
    println!("Verified as {} ({})", user.display_name, user.account_id);

    let store = CredentialStore::open_default();
    store.remember_account(BITBUCKET_SERVICE, &email)?;
    let via = store.store(BITBUCKET_SERVICE, &email, &token)?;
    let via_label = match via {
        StoredVia::Keyring => "the OS keyring",
        StoredVia::FileFallback => "a locked-down local file (see the warning above for why)",
    };
    println!("Stored in {via_label}. Run `autoreview auth status` any time to check.");
    Ok(())
}

fn run_auth_login_github(repo_root: &Path) -> anyhow::Result<()> {
    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    let Some(client_id) = config.auth.github.client_id else {
        anyhow::bail!(
            "no GitHub OAuth client_id configured — set auth.github.clientId in .autoreview/config.yaml to a Device-Flow-enabled OAuth App's client_id. \
             Register one (free, one-time) at https://github.com/settings/developers — see \
             https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow for how to enable device flow on it."
        );
    };

    let mut stdout = std::io::stdout();
    let token = autoreview_core::run_device_flow(&client_id, GITHUB_OAUTH_SCOPE, "curl", &mut stdout)?;
    println!("Authorized.");

    let store = CredentialStore::open_default();
    let via = store.store(GITHUB_SERVICE, GITHUB_ACCOUNT, &token)?;
    let via_label = match via {
        StoredVia::Keyring => "the OS keyring",
        StoredVia::FileFallback => "a locked-down local file (see the warning above for why)",
    };
    println!("Stored in {via_label}. Run `autoreview auth status` any time to check.");
    Ok(())
}

pub fn run_auth_login(repo_root: &Path, provider: &str, email: Option<String>, token: Option<String>) -> anyhow::Result<()> {
    match provider {
        "bitbucket" => run_auth_login_bitbucket(email, token),
        "github" => run_auth_login_github(repo_root),
        other => anyhow::bail!("unknown or not-yet-supported provider '{other}' — expected one of: {}", KNOWN_LOGIN_PROVIDERS.join(", ")),
    }
}

/// Removes the *local* credential only — neither provider's token gets
/// server-side-revoked by this command. For GitHub specifically, that's
/// not a corner cut but a real constraint of the device-flow model this
/// project deliberately chose: a public client (no `client_secret`) has
/// no credential to authenticate a call to GitHub's revoke endpoint
/// with. The message says so explicitly rather than implying the token
/// is dead.
pub fn run_auth_logout(provider: &str) -> anyhow::Result<()> {
    let store = CredentialStore::open_default();
    match provider {
        "github" => {
            store.delete(GITHUB_SERVICE, GITHUB_ACCOUNT)?;
            println!("Removed the locally stored GitHub credential.");
            println!(
                "Note: this does NOT revoke the token on GitHub's side — a device-flow app like this one has no client_secret to call GitHub's revoke API with. To fully revoke it, visit https://github.com/settings/applications and remove autoreview's authorization there."
            );
        }
        "bitbucket" => {
            match store.recall_account(BITBUCKET_SERVICE) {
                Some(email) => {
                    store.delete(BITBUCKET_SERVICE, &email)?;
                    store.forget_account(BITBUCKET_SERVICE)?;
                    println!("Removed the locally stored Bitbucket credential for {email}.");
                }
                None => println!("No Bitbucket credential was stored locally."),
            }
            println!("Note: this does NOT revoke the API token on Bitbucket's side — visit id.atlassian.com to revoke it there if you want to.");
        }
        other => anyhow::bail!("unknown provider '{other}' — expected one of: {}", KNOWN_LOGIN_PROVIDERS.join(", ")),
    }
    Ok(())
}
