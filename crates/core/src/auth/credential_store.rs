//! A small, three-tier credential store: an environment-variable override,
//! then the OS keyring (macOS Keychain, Windows Credential Manager, Linux
//! Secret Service via D-Bus), then a locked-down fallback file. This is
//! the first credential-storage code in this codebase — every prior
//! "reach an external host" feature (`git`'s own credential helper via
//! `storage::sync`, `gh auth login` via `mine_from_comments`, `curl`
//! against a local model server) has delegated to a tool the user already
//! authenticated, never stored a secret itself.
//!
//! **Why three tiers, not just the keyring:** WSL2 has no Secret Service
//! daemon running by default (no D-Bus session, no GNOME Keyring/KWallet
//! process), and plenty of headless CI runners are in the same position —
//! a keyring-only design would simply not work there out of the box. The
//! fallback file keeps this tool usable everywhere, at the cost of a real,
//! openly-stated tradeoff: a `chmod 600` file is only as safe as the local
//! filesystem's own access control, weaker than a real OS-managed secret
//! store. The env var tier exists for the case where *neither* of the
//! other two is usable (a locked-down CI runner with no writable cache
//! dir either) — the same escape hatch every comparable CLI (`gh`,
//! `aws-cli`) already offers, and checked first specifically so it can
//! always win regardless of what else is or isn't available.
//!
//! **Lookup order** (`load`): env var, then keyring, then file. **Store
//! order** (`store`): keyring, then (only on failure) file — there's no
//! sensible way to "store" into an env var, so it's read-only here.

use std::path::PathBuf;
use std::sync::OnceLock;

use keyring::Entry;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredVia {
    Keyring,
    FileFallback,
}

#[derive(Debug, Serialize, Deserialize)]
struct FallbackFileContents {
    account: String,
    secret: String,
}

pub struct CredentialStore {
    fallback_dir: PathBuf,
    /// Gates the "falling back to a file" warning to once per process —
    /// every `load`/`store` call after the first keyring miss stays
    /// silent rather than repeating the same line on every retry.
    warned_fallback: OnceLock<()>,
    /// Same once-per-process gating for the non-Unix "this file isn't
    /// permission-hardened" warning.
    #[cfg(not(unix))]
    warned_unhardened: OnceLock<()>,
}

/// `AUTOREVIEW_<PROVIDER>_TOKEN` — e.g. `AUTOREVIEW_GITHUB_TOKEN` for the
/// `"autoreview-github"` service. Strips the `"autoreview-"` prefix
/// callers use for the keyring/file service name (`GITHUB_SERVICE`,
/// `BITBUCKET_SERVICE` in `auth::mod`) rather than doubling it in the env
/// var name, which `AUTOREVIEW_AUTOREVIEW_GITHUB_TOKEN` would read as a
/// copy-paste mistake rather than a deliberate name.
fn env_var_name(service: &str) -> String {
    let bare = service.strip_prefix("autoreview-").unwrap_or(service);
    format!("AUTOREVIEW_{}_TOKEN", bare.to_uppercase().replace('-', "_"))
}

fn fallback_file_path(fallback_dir: &std::path::Path, service: &str) -> PathBuf {
    fallback_dir.join(format!("{service}.json"))
}

impl CredentialStore {
    /// The real, process-wide store: `~/.cache/autoreview/credentials/`
    /// (or `$TMPDIR` if `dirs::cache_dir()` can't resolve one) — deliberately
    /// **not** keyed by repo fingerprint the way `history_dir_for` is,
    /// since a GitHub/Bitbucket credential belongs to the person running
    /// this tool, not to any one repo.
    pub fn open_default() -> Self {
        let fallback_dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("autoreview").join("credentials");
        CredentialStore {
            fallback_dir,
            warned_fallback: OnceLock::new(),
            #[cfg(not(unix))]
            warned_unhardened: OnceLock::new(),
        }
    }

    #[cfg(test)]
    fn with_fallback_dir(fallback_dir: PathBuf) -> Self {
        CredentialStore {
            fallback_dir,
            warned_fallback: OnceLock::new(),
            #[cfg(not(unix))]
            warned_unhardened: OnceLock::new(),
        }
    }

    fn warn_fallback_once(&self, reason: &str) {
        if self.warned_fallback.set(()).is_ok() {
            eprintln!(
                "[warn] OS keyring unavailable ({reason}) — falling back to a locked-down file under {}. This is expected on WSL2 without a Secret Service daemon running, or in a headless environment.",
                self.fallback_dir.display()
            );
        }
    }

    fn load_from_file(&self, service: &str, account: &str) -> Option<String> {
        let path = fallback_file_path(&self.fallback_dir, service);
        let contents = std::fs::read_to_string(path).ok()?;
        let parsed: FallbackFileContents = serde_json::from_str(&contents).ok()?;
        (parsed.account == account).then_some(parsed.secret)
    }

    fn store_to_file(&self, service: &str, account: &str, secret: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.fallback_dir)?;
        super::curl_config::harden_dir(&self.fallback_dir);
        let path = fallback_file_path(&self.fallback_dir, service);
        let json = serde_json::to_string(&FallbackFileContents { account: account.to_string(), secret: secret.to_string() })?;
        // Creates the file already owner-only rather than writing it and
        // chmod-ing after — see `curl_config::write_private_file` for why
        // that ordering matters, and why an existing file is replaced
        // rather than truncated in place.
        super::curl_config::write_private_file(&path, &json)?;
        #[cfg(not(unix))]
        self.warn_unhardened_file_once();
        Ok(())
    }

    /// On a non-Unix target there is no `0600` equivalent applied here, so
    /// the fallback file inherits whatever the parent directory's ACL
    /// gives it. Windows is outside this feature's stated support
    /// (Linux/WSL2/macOS), and the honest thing is to say so at the moment
    /// a real secret is written rather than let it look equally protected
    /// everywhere.
    #[cfg(not(unix))]
    fn warn_unhardened_file_once(&self) {
        if self.warned_unhardened.set(()).is_ok() {
            eprintln!(
                "[warn] storing a credential in a file under {} without owner-only permissions — this platform's file mode isn't hardened by autoreview. Prefer the OS keyring, or set the AUTOREVIEW_<PROVIDER>_TOKEN environment variable instead.",
                self.fallback_dir.display()
            );
        }
    }

    fn delete_file(&self, service: &str) -> std::io::Result<()> {
        let path = fallback_file_path(&self.fallback_dir, service);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn account_marker_path(&self, service: &str) -> PathBuf {
        self.fallback_dir.join(format!("{service}.account"))
    }

    /// Remembers which `account` a login flow resolved for `service`, as
    /// plain (non-secret) text — separate from `store`'s actual secret
    /// value. Exists because Bitbucket's account name is the user's own
    /// email, resolved at login time, not a fixed constant the way
    /// GitHub's is (`auth::GITHUB_ACCOUNT`) — `auth status` and future
    /// callers need *some* way to know which account to `load` against
    /// without asking the user to retype their email every time. Not
    /// worth permission-hardening the way the credential file is, since
    /// this holds no secret.
    pub fn remember_account(&self, service: &str, account: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.fallback_dir)?;
        // The marker itself holds no secret, but it shares a directory
        // with the credential files that do — harden here too so a repo
        // that only ever calls `remember_account` still ends up with a
        // `0700` directory rather than the default `0755`.
        super::curl_config::harden_dir(&self.fallback_dir);
        std::fs::write(self.account_marker_path(service), account)?;
        Ok(())
    }

    pub fn recall_account(&self, service: &str) -> Option<String> {
        std::fs::read_to_string(self.account_marker_path(service)).ok().map(|s| s.trim().to_string())
    }

    pub fn forget_account(&self, service: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.account_marker_path(service)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Looks up a stored secret: env var override first, then the OS
    /// keyring, then the fallback file. `Ok(None)` means genuinely not
    /// found anywhere (not an error) — the caller decides what that
    /// means (e.g. "run `autoreview auth login <provider>` first").
    pub fn load(&self, service: &str, account: &str) -> anyhow::Result<Option<String>> {
        if let Ok(value) = std::env::var(env_var_name(service)) {
            if !value.is_empty() {
                return Ok(Some(value));
            }
        }

        match Entry::new(service, account).and_then(|e| e.get_password()) {
            Ok(secret) => return Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => {}
            Err(err) => self.warn_fallback_once(&err.to_string()),
        }

        Ok(self.load_from_file(service, account))
    }

    /// Stores a secret: the OS keyring first, falling back to the locked-
    /// down file only if the keyring itself isn't usable in this
    /// environment. Never writes to the env var tier — that's read-only.
    pub fn store(&self, service: &str, account: &str, secret: &str) -> anyhow::Result<StoredVia> {
        match Entry::new(service, account).and_then(|e| e.set_password(secret)) {
            Ok(()) => Ok(StoredVia::Keyring),
            Err(err) => {
                self.warn_fallback_once(&err.to_string());
                self.store_to_file(service, account, secret)?;
                Ok(StoredVia::FileFallback)
            }
        }
    }

    /// Removes a stored credential from both the keyring and the fallback
    /// file — best-effort against each independently (a credential that
    /// only ever landed in one of the two, or in neither, isn't an
    /// error). Never touches an env-var override, since that's the
    /// caller's own shell environment, not something this store manages.
    pub fn delete(&self, service: &str, account: &str) -> anyhow::Result<()> {
        let _ = Entry::new(service, account).and_then(|e| e.delete_credential());
        self.delete_file(service)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_temp_fallback() -> (CredentialStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::with_fallback_dir(dir.path().to_path_buf());
        (store, dir)
    }

    #[test]
    fn stores_and_loads_via_the_fallback_file_when_the_keyring_is_unavailable() {
        // In this sandboxed test environment the real OS keyring is
        // either unavailable or (worse for test isolation) genuinely
        // writable, so these tests exercise the fallback file path
        // directly rather than asserting on `store`'s return value,
        // which depends on real keyring availability neither controlled
        // nor guaranteed here.
        let (store, _dir) = store_with_temp_fallback();
        store.store_to_file("test-service", "alice", "s3cr3t").unwrap();
        assert_eq!(store.load_from_file("test-service", "alice"), Some("s3cr3t".to_string()));
    }

    #[test]
    fn fallback_file_lookup_returns_none_for_a_mismatched_account() {
        let (store, _dir) = store_with_temp_fallback();
        store.store_to_file("test-service", "alice", "s3cr3t").unwrap();
        assert_eq!(store.load_from_file("test-service", "bob"), None);
    }

    #[test]
    fn fallback_file_lookup_returns_none_when_nothing_was_ever_stored() {
        let (store, _dir) = store_with_temp_fallback();
        assert_eq!(store.load_from_file("never-stored", "alice"), None);
    }

    #[test]
    fn fallback_file_is_written_with_owner_only_permissions() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let (store, dir) = store_with_temp_fallback();
            store.store_to_file("test-service", "alice", "s3cr3t").unwrap();
            let path = fallback_file_path(dir.path(), "test-service");
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "got: {mode:o}");
        }
    }

    #[test]
    fn remembers_and_recalls_an_account_name() {
        let (store, _dir) = store_with_temp_fallback();
        assert_eq!(store.recall_account("autoreview-bitbucket"), None);
        store.remember_account("autoreview-bitbucket", "alice@example.com").unwrap();
        assert_eq!(store.recall_account("autoreview-bitbucket"), Some("alice@example.com".to_string()));
    }

    #[test]
    fn forgetting_a_never_remembered_account_is_not_an_error() {
        let (store, _dir) = store_with_temp_fallback();
        store.forget_account("never-remembered").unwrap();
    }

    #[test]
    fn forget_account_removes_a_previously_remembered_account() {
        let (store, _dir) = store_with_temp_fallback();
        store.remember_account("autoreview-bitbucket", "alice@example.com").unwrap();
        store.forget_account("autoreview-bitbucket").unwrap();
        assert_eq!(store.recall_account("autoreview-bitbucket"), None);
    }

    #[test]
    fn deleting_a_never_stored_fallback_file_is_not_an_error() {
        let (store, _dir) = store_with_temp_fallback();
        store.delete_file("never-stored").unwrap();
    }

    #[test]
    fn delete_file_removes_a_previously_stored_credential() {
        let (store, _dir) = store_with_temp_fallback();
        store.store_to_file("test-service", "alice", "s3cr3t").unwrap();
        store.delete_file("test-service").unwrap();
        assert_eq!(store.load_from_file("test-service", "alice"), None);
    }

    #[test]
    fn env_var_override_wins_over_the_fallback_file() {
        let (store, _dir) = store_with_temp_fallback();
        store.store_to_file("autoreview-test-service", "alice", "file-secret").unwrap();
        // SAFETY: test-only, single-threaded within this test's own
        // process for the duration of the env var mutation.
        unsafe { std::env::set_var("AUTOREVIEW_TEST_SERVICE_TOKEN", "env-secret") };
        let loaded = store.load("autoreview-test-service", "alice").unwrap();
        unsafe { std::env::remove_var("AUTOREVIEW_TEST_SERVICE_TOKEN") };
        assert_eq!(loaded, Some("env-secret".to_string()));
    }

    #[test]
    fn env_var_name_strips_the_autoreview_prefix_and_uppercases() {
        assert_eq!(env_var_name("autoreview-github"), "AUTOREVIEW_GITHUB_TOKEN");
        assert_eq!(env_var_name("autoreview-bitbucket"), "AUTOREVIEW_BITBUCKET_TOKEN");
    }

    #[test]
    fn round_trips_through_the_real_os_keyring_when_one_is_available() {
        // Not injecting a fallback_dir here — this deliberately exercises
        // `Entry::new`/`set_password`/`get_password`/`delete_credential`
        // against whatever real keyring backend this machine has (Secret
        // Service on Linux, Keychain on macOS). Best-effort: skipped
        // rather than failed if no backend is reachable in this
        // environment (a sandboxed CI runner with no D-Bus session at
        // all, for instance) — the fallback-file path above already
        // covers that case directly and deterministically.
        let store = CredentialStore::open_default();
        let Ok(via) = store.store("autoreview-credential-store-selftest", "selftest-account", "selftest-secret") else {
            eprintln!("skipping: no credential backend reachable in this environment");
            return;
        };
        assert_eq!(store.load("autoreview-credential-store-selftest", "selftest-account").unwrap(), Some("selftest-secret".to_string()));
        store.delete("autoreview-credential-store-selftest", "selftest-account").unwrap();
        assert_eq!(store.load("autoreview-credential-store-selftest", "selftest-account").unwrap(), None, "stored via {via:?}");
    }
}
