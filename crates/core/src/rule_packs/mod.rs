//! Registration and resolution for external "rule packs" — third-party
//! rule bundles a repo can point autoreview at without forking the binary.
//! Registration lives in `.autoreview/rulepacks.yaml`
//! (`load_rule_packs_config`, opt-in via file presence — same convention
//! `architecture.yaml` already established: no file means no packs
//! registered, not an error). Each registered pack is resolved to a real
//! local directory (`resolve_rule_packs`) and validated against its own
//! `rulepack.yaml` before it's handed to the rule-loading backends
//! (`analyzers::ast_grep`/`taint_rules`/`threshold_rules`), which fold a
//! resolved pack's directory into the same rule tree the embedded builtin
//! pack is read from — a pack's rules always *run* exactly like builtin
//! ones (same category/severity/kind dispatch), but `RulePackConfig::trust`
//! decides whether their findings surface: `full` (default) surfaces them
//! immediately, `shadow` suppresses them the same way a human-authored
//! `.autoreview/rules/shadow/` rule would (`diff.rs` reads
//! `ResolvedRulePack::trust` to apply this).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use autoreview_schema::{RulePackConfig, RulePackManifest, RulePackSourceConfig, RulePackTrust, RulePacksFile};

use crate::storage::sync::run_git;

const RULEPACKS_FILE_NAME: &str = "rulepacks.yaml";
/// A pack's own self-identity file at its root — not a rule file, so rule
/// discovery (`analyzers::ast_grep`'s disk-root walk) must skip it even
/// though it's a `.yaml` file sitting in the same tree it recursively scans.
pub(crate) const MANIFEST_FILE_NAME: &str = "rulepack.yaml";

/// Reads and parses `.autoreview/rulepacks.yaml`. Returns an empty list
/// (not an error) when the file doesn't exist — registering rule packs is
/// entirely opt-in.
pub fn load_rule_packs_config(path: &Path) -> anyhow::Result<Vec<RulePackConfig>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let file: RulePacksFile = serde_yaml::from_str(&contents)?;
            Ok(file.packs)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(err) => Err(err.into()),
    }
}

/// The conventional path for a repo's rule-pack registration file,
/// `.autoreview/rulepacks.yaml` — matches `architecture.yaml`'s own fixed-
/// path convention rather than being searched/discovered.
pub fn rule_packs_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".autoreview").join(RULEPACKS_FILE_NAME)
}

/// The real, machine-wide cache root a `kind: git` pack source clones
/// into — `~/.cache/autoreview/`, the same base `history_dir_for` (crate
/// `cli`) uses for its own per-repo history cache, so rule-pack clones and
/// review-history data sit side by side under one cache root. Kept as a
/// separate function callers pass explicitly (see `resolve_rule_packs`)
/// rather than hardcoded inside the resolver itself, so tests can point it
/// at a tempdir instead of touching the real machine cache.
pub fn default_rule_packs_cache_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("autoreview")
}

/// A rule pack resolved to a real, readable local directory — the shape
/// every rule-loading backend (ast-grep pattern rules, taint rules,
/// threshold rules) consumes, regardless of whether the pack's declared
/// source was `local` or `git` (git resolution produces this same shape,
/// by design, so downstream loading logic never needs to know which kind
/// of source a pack came from).
#[derive(Debug)]
pub struct ResolvedRulePack {
    pub id: String,
    pub local_path: PathBuf,
    /// Carried through from the pack's `rulepacks.yaml` registration
    /// (`RulePackConfig::trust`) — `diff.rs` reads this to decide whether
    /// this pack's findings surface directly or get suppressed like a
    /// shadow-mode rule.
    pub trust: RulePackTrust,
}

/// Resolves every configured pack against `cache_root` (see
/// `default_rule_packs_cache_root`; only consulted for `kind: git`
/// sources). A pack that fails to resolve (missing directory, unreadable/
/// mismatched manifest, unreachable git remote) is reported alongside the
/// id it belongs to rather than aborting the whole call — the caller
/// decides how to surface a failure (Stage 1 prints a warning and
/// continues, matching how a malformed `architecture.yaml` degrades to a
/// warning rather than aborting the whole review), so one misconfigured
/// pack doesn't silently drop every other registered pack too.
pub fn resolve_rule_packs(repo_root: &Path, cache_root: &Path, configured: &[RulePackConfig]) -> Vec<(String, anyhow::Result<ResolvedRulePack>)> {
    configured.iter().map(|pack| (pack.id.clone(), resolve_rule_pack(repo_root, cache_root, pack))).collect()
}

fn resolve_rule_pack(repo_root: &Path, cache_root: &Path, pack: &RulePackConfig) -> anyhow::Result<ResolvedRulePack> {
    let local_path = match &pack.source {
        RulePackSourceConfig::Local { path } => resolve_local_source(repo_root, path)?,
        RulePackSourceConfig::Git { url, r#ref, subpath } => resolve_git_source(cache_root, url, r#ref.as_deref(), subpath.as_deref())?,
    };
    validate_pack_manifest(&pack.id, &local_path)?;
    Ok(ResolvedRulePack { id: pack.id.clone(), local_path, trust: pack.trust })
}

fn resolve_local_source(repo_root: &Path, path: &str) -> anyhow::Result<PathBuf> {
    let resolved = repo_root.join(path);
    if !resolved.is_dir() {
        anyhow::bail!("local source '{path}' does not resolve to a directory: {}", resolved.display());
    }
    Ok(resolved)
}

/// A stable, filesystem-safe cache directory for a `(url, ref)` pair under
/// `cache_root` — keyed by content, not by the local `id` a
/// `rulepacks.yaml` happens to register a pack under, so two repos
/// registering the same URL under different ids share one clone, and two
/// different sources that happen to share an id never collide.
fn git_cache_dir(cache_root: &Path, url: &str, r#ref: Option<&str>) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    if let Some(r) = r#ref {
        hasher.update(b"@");
        hasher.update(r.as_bytes());
    }
    let digest = hasher.finalize();
    let fingerprint = format!("{digest:x}")[..16].to_string();
    cache_root.join("rulepacks").join(fingerprint)
}

/// Clones (or, on a repeat run, fetches into) a shared local cache under
/// `cache_root` and checks out `ref` (default: the remote's default
/// branch, via `HEAD`) — same shallow single-ref approach as
/// `storage::sync`'s own git usage, and the same `--` guard before a
/// repo-local-config-supplied value (`url`/`ref`) to block flag/argument
/// injection from a value that could itself be attacker-controlled if
/// `.autoreview/rulepacks.yaml` came from a cloned, untrusted repo.
fn resolve_git_source(cache_root: &Path, url: &str, r#ref: Option<&str>, subpath: Option<&str>) -> anyhow::Result<PathBuf> {
    let cache_dir = git_cache_dir(cache_root, url, r#ref);
    if !cache_dir.join(".git").exists() {
        std::fs::create_dir_all(&cache_dir)?;
        run_git(&cache_dir, &["init", "-q"])?;
        run_git(&cache_dir, &["remote", "add", "origin", "--", url])?;
    }
    let fetch_ref = r#ref.unwrap_or("HEAD");
    run_git(&cache_dir, &["fetch", "--depth", "1", "origin", "--", fetch_ref])?;
    run_git(&cache_dir, &["checkout", "-q", "--detach", "FETCH_HEAD"])?;

    let root = match subpath {
        Some(sp) => cache_dir.join(sp),
        None => cache_dir,
    };
    if !root.is_dir() {
        anyhow::bail!("cloned but subpath '{}' is not a directory in the repo", subpath.unwrap_or(""));
    }
    Ok(root)
}

/// Resolves `source` without an id to check against — the reverse of
/// `resolve_rule_pack`, used by `autoreview rules packs add <source>`
/// before the pack has been registered under any id at all. Returns the
/// pack's own self-declared id (read from its `rulepack.yaml`) alongside
/// the local path it resolved to, so the caller can register it under
/// that id.
pub fn discover_pack_source(repo_root: &Path, cache_root: &Path, source: &RulePackSourceConfig) -> anyhow::Result<(String, PathBuf)> {
    let local_path = match source {
        RulePackSourceConfig::Local { path } => resolve_local_source(repo_root, path)?,
        RulePackSourceConfig::Git { url, r#ref, subpath } => resolve_git_source(cache_root, url, r#ref.as_deref(), subpath.as_deref())?,
    };
    let manifest_path = local_path.join(MANIFEST_FILE_NAME);
    let contents = std::fs::read_to_string(&manifest_path).map_err(|err| anyhow::anyhow!("failed to read {}: {err}", manifest_path.display()))?;
    let manifest: RulePackManifest = serde_yaml::from_str(&contents).map_err(|err| anyhow::anyhow!("failed to parse {}: {err}", manifest_path.display()))?;
    Ok((manifest.id, local_path))
}

/// Writes `.autoreview/rulepacks.yaml`, creating the parent directory if
/// needed — the write-side counterpart to `load_rule_packs_config`.
pub fn save_rule_packs_config(path: &Path, file: &RulePacksFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(file)?;
    std::fs::write(path, yaml)?;
    Ok(())
}

/// Reads `<local_path>/rulepack.yaml` and checks its declared `id` matches
/// `expected_id` (the id it was registered under in `rulepacks.yaml`) —
/// catches "pointed at the wrong directory" early with a clear error
/// rather than silently loading rules under a mismatched identity.
fn validate_pack_manifest(expected_id: &str, local_path: &Path) -> anyhow::Result<()> {
    let manifest_path = local_path.join(MANIFEST_FILE_NAME);
    let contents = std::fs::read_to_string(&manifest_path).map_err(|err| anyhow::anyhow!("failed to read {}: {err}", manifest_path.display()))?;
    let manifest: RulePackManifest = serde_yaml::from_str(&contents).map_err(|err| anyhow::anyhow!("failed to parse {}: {err}", manifest_path.display()))?;
    if manifest.id != expected_id {
        anyhow::bail!("registered as '{expected_id}' but its rulepack.yaml declares id '{}' — check the path in rulepacks.yaml", manifest.id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn load_rule_packs_config_returns_empty_when_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_rule_packs_config(&dir.path().join("rulepacks.yaml")).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn load_rule_packs_config_parses_the_documented_yaml_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rulepacks.yaml");
        write(&path, "packs:\n  - id: acme-security\n    source:\n      kind: local\n      path: ../shared-rules/acme-security\n");
        let packs = load_rule_packs_config(&path).unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "acme-security");
    }

    /// Local-source tests never touch a git cache, so a throwaway tempdir
    /// stands in for `cache_root`.
    fn unused_cache_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn resolves_a_valid_local_pack() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        write(&root.path().join("shared/acme-security/rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n");
        let config = RulePackConfig { id: "acme-security".to_string(), source: RulePackSourceConfig::Local { path: "../shared/acme-security".to_string() }, trust: RulePackTrust::Full };
        let results = resolve_rule_packs(&repo_root, unused_cache_root().path(), &[config]);
        assert_eq!(results.len(), 1);
        let (id, result) = &results[0];
        assert_eq!(id, "acme-security");
        let resolved = result.as_ref().unwrap();
        assert_eq!(resolved.id, "acme-security");
        assert!(resolved.local_path.join("rulepack.yaml").exists());
    }

    #[test]
    fn a_missing_local_directory_fails_that_pack_without_touching_others() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        write(&root.path().join("shared/good/rulepack.yaml"), "id: good\nversion: \"1.0.0\"\n");
        let configs = vec![
            RulePackConfig { id: "missing".to_string(), source: RulePackSourceConfig::Local { path: "../shared/does-not-exist".to_string() }, trust: RulePackTrust::Full },
            RulePackConfig { id: "good".to_string(), source: RulePackSourceConfig::Local { path: "../shared/good".to_string() }, trust: RulePackTrust::Full },
        ];
        let results = resolve_rule_packs(&repo_root, unused_cache_root().path(), &configs);
        assert!(results[0].1.is_err());
        assert!(results[1].1.is_ok());
    }

    #[test]
    fn a_manifest_id_mismatch_is_a_clear_error() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        write(&root.path().join("shared/acme-security/rulepack.yaml"), "id: totally-different-id\nversion: \"1.0.0\"\n");
        let config = RulePackConfig { id: "acme-security".to_string(), source: RulePackSourceConfig::Local { path: "../shared/acme-security".to_string() }, trust: RulePackTrust::Full };
        let results = resolve_rule_packs(&repo_root, unused_cache_root().path(), &[config]);
        let err = results[0].1.as_ref().unwrap_err();
        assert!(err.to_string().contains("totally-different-id"), "got: {err}");
    }

    #[test]
    fn a_missing_manifest_file_is_a_clear_error() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::create_dir_all(root.path().join("shared/no-manifest")).unwrap();
        let config = RulePackConfig { id: "no-manifest".to_string(), source: RulePackSourceConfig::Local { path: "../shared/no-manifest".to_string() }, trust: RulePackTrust::Full };
        let results = resolve_rule_packs(&repo_root, unused_cache_root().path(), &[config]);
        assert!(results[0].1.is_err());
    }

    fn run(cwd: &Path, args: &[&str]) {
        let status = std::process::Command::new("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    /// A minimal, real local git repo (no network) acting as a "remote" a
    /// rule pack's `kind: git` source points at — network-free,
    /// deterministic, no real cache-dir dependency.
    fn init_remote_with_pack(id: &str) -> tempfile::TempDir {
        let remote = tempfile::tempdir().unwrap();
        run(remote.path(), &["init", "-q"]);
        run(remote.path(), &["config", "user.email", "t@t.com"]);
        run(remote.path(), &["config", "user.name", "T"]);
        write(&remote.path().join("rulepack.yaml"), &format!("id: {id}\nversion: \"1.0.0\"\n"));
        write(&remote.path().join("go/security/no-println.yml"), "id: acme-no-println\nlanguage: Go\ncategory: security\nseverity: error\nmessage: no println\nrule:\n  pattern: println($$$ARGS)\n");
        run(remote.path(), &["add", "-A"]);
        run(remote.path(), &["commit", "-q", "-m", "init"]);
        remote
    }

    #[test]
    fn resolves_a_git_source_by_cloning_and_checking_out_the_manifest() {
        let remote = init_remote_with_pack("acme-git-pack");
        let url = remote.path().to_string_lossy().to_string();
        let config = RulePackConfig { id: "acme-git-pack".to_string(), source: RulePackSourceConfig::Git { url, r#ref: None, subpath: None }, trust: RulePackTrust::Full };
        let cache_root = tempfile::tempdir().unwrap();
        let results = resolve_rule_packs(tempfile::tempdir().unwrap().path(), cache_root.path(), &[config]);
        let (id, result) = &results[0];
        assert_eq!(id, "acme-git-pack");
        let resolved = result.as_ref().unwrap_or_else(|e| panic!("expected Ok, got {e}"));
        assert!(resolved.local_path.join("rulepack.yaml").exists());
        assert!(resolved.local_path.join("go/security/no-println.yml").exists());
        // Confirms the clone actually landed under the given cache root,
        // not the real machine cache.
        assert!(resolved.local_path.starts_with(cache_root.path()));
    }

    #[test]
    fn a_git_source_id_mismatch_is_a_clear_error() {
        let remote = init_remote_with_pack("actual-id");
        let url = remote.path().to_string_lossy().to_string();
        let config = RulePackConfig { id: "expected-id".to_string(), source: RulePackSourceConfig::Git { url, r#ref: None, subpath: None }, trust: RulePackTrust::Full };
        let results = resolve_rule_packs(tempfile::tempdir().unwrap().path(), unused_cache_root().path(), &[config]);
        let err = results[0].1.as_ref().unwrap_err();
        assert!(err.to_string().contains("actual-id"), "got: {err}");
    }

    #[test]
    fn discover_pack_source_reads_the_id_from_a_local_manifest() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        write(&root.path().join("shared/acme-security/rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n");
        let source = RulePackSourceConfig::Local { path: "../shared/acme-security".to_string() };
        let (id, local_path) = discover_pack_source(&repo_root, unused_cache_root().path(), &source).unwrap();
        assert_eq!(id, "acme-security");
        assert!(local_path.join("rulepack.yaml").exists());
    }

    #[test]
    fn discover_pack_source_reads_the_id_from_a_git_manifest() {
        let remote = init_remote_with_pack("acme-git-pack");
        let url = remote.path().to_string_lossy().to_string();
        let source = RulePackSourceConfig::Git { url, r#ref: None, subpath: None };
        let cache_root = tempfile::tempdir().unwrap();
        let (id, local_path) = discover_pack_source(tempfile::tempdir().unwrap().path(), cache_root.path(), &source).unwrap();
        assert_eq!(id, "acme-git-pack");
        assert!(local_path.join("rulepack.yaml").exists());
    }

    #[test]
    fn discover_pack_source_fails_clearly_when_the_manifest_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::create_dir_all(root.path().join("shared/no-manifest")).unwrap();
        let source = RulePackSourceConfig::Local { path: "../shared/no-manifest".to_string() };
        let err = discover_pack_source(&repo_root, unused_cache_root().path(), &source).unwrap_err();
        assert!(err.to_string().contains("rulepack.yaml"), "got: {err}");
    }

    #[test]
    fn save_and_load_rule_packs_config_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".autoreview").join("rulepacks.yaml");
        let file = RulePacksFile { packs: vec![RulePackConfig { id: "acme-security".to_string(), source: RulePackSourceConfig::Local { path: "../shared/acme-security".to_string() }, trust: RulePackTrust::Full }] };
        save_rule_packs_config(&path, &file).unwrap();
        let loaded = load_rule_packs_config(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "acme-security");
    }

    #[test]
    fn an_unreachable_git_url_fails_that_pack_clearly() {
        let root = tempfile::tempdir().unwrap();
        let bogus_url = root.path().join("does-not-exist-as-a-repo").to_string_lossy().to_string();
        let config = RulePackConfig { id: "acme-git-pack".to_string(), source: RulePackSourceConfig::Git { url: bogus_url, r#ref: None, subpath: None }, trust: RulePackTrust::Full };
        let results = resolve_rule_packs(root.path(), unused_cache_root().path(), &[config]);
        assert!(results[0].1.is_err());
    }
}
