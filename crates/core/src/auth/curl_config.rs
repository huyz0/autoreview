//! Keeps credentials out of curl's process arguments.
//!
//! Every call site here previously passed the secret as an argv entry
//! (`-u "email:token"`, `-H "Authorization: Bearer ..."`). On Linux
//! `/proc/<pid>/cmdline` is world-readable, so any other user on the
//! machine could read the token straight out of `ps aux` for as long as
//! the request ran. `bitbucket_token`'s own doc comment used to claim `-u`
//! kept the token "out of anything that might get logged by mistake" —
//! true as far as it went, but it never addressed process listing, which
//! is the bigger exposure on a shared host.
//!
//! curl's `--config <file>` reads the same options from a file instead, so
//! the secret only ever exists in a file this module creates with mode
//! `0600` and deletes on drop. Verified against the real Bitbucket and
//! OpenRouter APIs that both `user = ` (HTTP Basic) and `header = ` are
//! honored this way — a fake credential still gets a real 401, so the
//! credential genuinely reaches the server rather than being silently
//! dropped.
//!
//! Not used via `--config -` (stdin) on purpose: `agents::openai_compatible`
//! already pipes its JSON request body through stdin, and there is only one
//! of those. A short-lived `0600` file works for every call site uniformly.

use std::io::Write;
use std::path::{Path, PathBuf};

/// A curl config file holding one credential, removed from disk when this
/// value drops. Hold it alive for exactly as long as the `Command` that
/// references `path()`.
#[derive(Debug)]
pub struct CurlAuthConfig {
    path: PathBuf,
}

/// curl's config parser treats `\` and `"` specially inside a quoted
/// value, so both have to be escaped or a token containing either would be
/// silently mangled into a different (wrong) credential.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

impl CurlAuthConfig {
    fn write(contents: &str) -> std::io::Result<Self> {
        let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("autoreview").join("curl");
        std::fs::create_dir_all(&dir)?;
        harden_dir(&dir);
        sweep_stale(&dir);

        // Unique per call: concurrent requests (a paginated Bitbucket scan
        // racing anything else) must not share or clobber one file.
        let path = dir.join(format!("auth-{}-{}.conf", std::process::id(), next_sequence()));
        write_private_file(&path, contents)?;
        Ok(CurlAuthConfig { path })
    }

    /// HTTP Basic auth — curl's `user = "name:password"`.
    pub fn basic(user: &str, password: &str) -> std::io::Result<Self> {
        Self::write(&format!("user = \"{}:{}\"\n", escape(user), escape(password)))
    }

    /// A bearer token — curl's `header = "Authorization: Bearer ..."`.
    pub fn bearer(token: &str) -> std::io::Result<Self> {
        Self::write(&format!("header = \"Authorization: Bearer {}\"\n", escape(token)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CurlAuthConfig {
    fn drop(&mut self) {
        // Best-effort: a leftover 0600 file in the user's own cache dir is
        // not worth failing a review run over, and there is nothing useful
        // to do with the error at this point.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// How long a config file must be untouched before a later run treats it
/// as abandoned. Comfortably longer than any single curl call this
/// codebase makes (the longest `--max-time` is 120s), so this can never
/// delete a file another live process is still using.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

/// Removes config files left behind by a process that died before its
/// `Drop` could run — `Drop` does not execute on SIGKILL, a panic=abort,
/// or a hard power loss, so without this a crash would strand a file
/// containing a live credential in the cache directory indefinitely.
/// Best-effort throughout: a failure here must never break an actual
/// request, and the files are `0600` in a `0700` directory regardless.
///
/// `stale_after` is a parameter rather than reading `STALE_AFTER`
/// directly so tests can exercise both branches deterministically,
/// without backdating file mtimes (which would need another dependency).
fn sweep_stale_older_than(dir: &Path, stale_after: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("conf") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
        if now.duration_since(modified).is_ok_and(|age| age > stale_after) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn sweep_stale(dir: &Path) {
    sweep_stale_older_than(dir, STALE_AFTER);
}

fn next_sequence() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Creates `path` already restricted to the owner and writes `contents`.
///
/// The mode is set **at creation time**, not with a `set_permissions` call
/// afterwards: `std::fs::write` followed by a chmod leaves a window where
/// the file exists on disk with the default mode (typically world-readable
/// `0644` after umask), and anything watching the directory can read the
/// secret during it.
///
/// Any existing file is removed first rather than truncated, because
/// `OpenOptions::mode` applies **only when the file is created** — opening
/// an existing world-readable file with `create(true).truncate(true)`
/// silently keeps its old, wider mode, which would quietly re-expose a
/// secret every time a credential was overwritten.
pub(crate) fn write_private_file(path: &Path, contents: &str) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }

    let mut options = std::fs::OpenOptions::new();
    // `create_new` so this fails loudly rather than reusing a file that
    // reappeared between the remove above and here — that file would not
    // be one this process created, and its mode is unknown.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Best-effort `0700` on a directory holding secrets. Failure is not fatal
/// — the files inside are individually `0600`, which is what actually
/// protects them; this only narrows directory listing.
pub(crate) fn harden_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_config_holds_the_credential_in_curls_own_syntax() {
        let config = CurlAuthConfig::basic("user@example.com", "s3cret").unwrap();
        let contents = std::fs::read_to_string(config.path()).unwrap();
        assert_eq!(contents, "user = \"user@example.com:s3cret\"\n");
    }

    #[test]
    fn bearer_config_holds_the_credential_in_curls_own_syntax() {
        let config = CurlAuthConfig::bearer("sk-abc123").unwrap();
        let contents = std::fs::read_to_string(config.path()).unwrap();
        assert_eq!(contents, "header = \"Authorization: Bearer sk-abc123\"\n");
    }

    /// A token containing a quote or backslash must survive intact — an
    /// unescaped one would end curl's quoted value early and send a
    /// silently different credential rather than failing loudly.
    #[test]
    fn quotes_and_backslashes_in_a_secret_are_escaped() {
        let config = CurlAuthConfig::bearer(r#"we"ird\token"#).unwrap();
        let contents = std::fs::read_to_string(config.path()).unwrap();
        assert_eq!(contents, "header = \"Authorization: Bearer we\\\"ird\\\\token\"\n");
    }

    #[test]
    fn the_config_file_is_removed_when_dropped() {
        let path = {
            let config = CurlAuthConfig::bearer("sk-abc123").unwrap();
            assert!(config.path().exists());
            config.path().to_path_buf()
        };
        assert!(!path.exists(), "the config file must not outlive the guard that owns it");
    }

    #[test]
    fn two_configs_never_share_a_path() {
        let a = CurlAuthConfig::bearer("token-a").unwrap();
        let b = CurlAuthConfig::bearer("token-b").unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(std::fs::read_to_string(a.path()).unwrap(), "header = \"Authorization: Bearer token-a\"\n");
        assert_eq!(std::fs::read_to_string(b.path()).unwrap(), "header = \"Authorization: Bearer token-b\"\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_config_file_is_owner_only_from_the_moment_it_exists() {
        use std::os::unix::fs::PermissionsExt;
        let config = CurlAuthConfig::bearer("sk-abc123").unwrap();
        let mode = std::fs::metadata(config.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "got {:o}", mode & 0o777);
    }

    /// A file left behind by a process killed before `Drop` ran must be
    /// cleaned up by a later run, not stranded with a live credential in
    /// it. Observed for real: SIGKILL-ing a request mid-flight left an
    /// `auth-*.conf` behind.
    #[test]
    fn sweep_removes_an_abandoned_config() {
        let dir = tempfile::tempdir().unwrap();
        let abandoned = dir.path().join("auth-999-0.conf");
        std::fs::write(&abandoned, "header = \"Authorization: Bearer old\"\n").unwrap();
        // Any nonzero age counts as stale under a zero threshold.
        std::thread::sleep(std::time::Duration::from_millis(10));

        sweep_stale_older_than(dir.path(), std::time::Duration::ZERO);

        assert!(!abandoned.exists(), "an abandoned config file must be swept");
    }

    /// The other half of the same guarantee: sweeping must never delete a
    /// file a concurrently-running request is still handing to curl.
    #[test]
    fn sweep_spares_a_config_younger_than_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("auth-999-1.conf");
        std::fs::write(&fresh, "header = \"Authorization: Bearer new\"\n").unwrap();

        sweep_stale_older_than(dir.path(), std::time::Duration::from_secs(3600));

        assert!(fresh.exists(), "a config file a live request may still be using must be spared");
    }

    #[test]
    fn sweep_ignores_non_config_files() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("not-ours.txt");
        std::fs::write(&other, "x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));

        sweep_stale_older_than(dir.path(), std::time::Duration::ZERO);

        assert!(other.exists(), "sweeping must be scoped to this module's own .conf files");
    }

    #[cfg(unix)]
    #[test]
    fn write_private_file_creates_an_owner_only_file_without_a_chmod_window() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        write_private_file(&path, "{\"secret\":\"x\"}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "got {:o}", mode & 0o777);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"secret\":\"x\"}");
    }

    /// Overwriting an existing credential must not silently widen its
    /// permissions back to the default.
    #[cfg(unix)]
    #[test]
    fn write_private_file_keeps_the_mode_when_overwriting() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private_file(&path, "new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "an existing world-readable file must be re-hardened, got {:o}", mode & 0o777);
    }
}
