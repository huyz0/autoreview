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
//! pack is read from — "full trust immediately," per the design: a
//! registered pack's rules run exactly like builtin rules, no shadow/
//! promoted staging gate.

use std::path::{Path, PathBuf};

use autoreview_schema::{RulePackConfig, RulePackManifest, RulePackSourceConfig, RulePacksFile};

const RULEPACKS_FILE_NAME: &str = "rulepacks.yaml";
const MANIFEST_FILE_NAME: &str = "rulepack.yaml";

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
}

/// Resolves every configured pack. A pack that fails to resolve (missing
/// directory, unreadable/mismatched manifest, or — once implemented — a
/// git source) is reported alongside the id it belongs to rather than
/// aborting the whole call — the caller decides how to surface a failure
/// (Stage 1 prints a warning and continues, matching how a malformed
/// `architecture.yaml` degrades to a warning rather than aborting the
/// whole review), so one misconfigured pack doesn't silently drop every
/// other registered pack too.
pub fn resolve_rule_packs(repo_root: &Path, configured: &[RulePackConfig]) -> Vec<(String, anyhow::Result<ResolvedRulePack>)> {
    configured.iter().map(|pack| (pack.id.clone(), resolve_rule_pack(repo_root, pack))).collect()
}

fn resolve_rule_pack(repo_root: &Path, pack: &RulePackConfig) -> anyhow::Result<ResolvedRulePack> {
    let local_path = match &pack.source {
        RulePackSourceConfig::Local { path } => resolve_local_source(repo_root, path)?,
        RulePackSourceConfig::Git { .. } => anyhow::bail!("rule pack '{}': git sources are not yet supported", pack.id),
    };
    validate_pack_manifest(&pack.id, &local_path)?;
    Ok(ResolvedRulePack { id: pack.id.clone(), local_path })
}

fn resolve_local_source(repo_root: &Path, path: &str) -> anyhow::Result<PathBuf> {
    let resolved = repo_root.join(path);
    if !resolved.is_dir() {
        anyhow::bail!("local source '{path}' does not resolve to a directory: {}", resolved.display());
    }
    Ok(resolved)
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

    #[test]
    fn resolves_a_valid_local_pack() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        write(&root.path().join("shared/acme-security/rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n");
        let config = RulePackConfig { id: "acme-security".to_string(), source: RulePackSourceConfig::Local { path: "../shared/acme-security".to_string() } };
        let results = resolve_rule_packs(&repo_root, &[config]);
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
            RulePackConfig { id: "missing".to_string(), source: RulePackSourceConfig::Local { path: "../shared/does-not-exist".to_string() } },
            RulePackConfig { id: "good".to_string(), source: RulePackSourceConfig::Local { path: "../shared/good".to_string() } },
        ];
        let results = resolve_rule_packs(&repo_root, &configs);
        assert!(results[0].1.is_err());
        assert!(results[1].1.is_ok());
    }

    #[test]
    fn a_manifest_id_mismatch_is_a_clear_error() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        write(&root.path().join("shared/acme-security/rulepack.yaml"), "id: totally-different-id\nversion: \"1.0.0\"\n");
        let config = RulePackConfig { id: "acme-security".to_string(), source: RulePackSourceConfig::Local { path: "../shared/acme-security".to_string() } };
        let results = resolve_rule_packs(&repo_root, &[config]);
        let err = results[0].1.as_ref().unwrap_err();
        assert!(err.to_string().contains("totally-different-id"), "got: {err}");
    }

    #[test]
    fn a_missing_manifest_file_is_a_clear_error() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::create_dir_all(root.path().join("shared/no-manifest")).unwrap();
        let config = RulePackConfig { id: "no-manifest".to_string(), source: RulePackSourceConfig::Local { path: "../shared/no-manifest".to_string() } };
        let results = resolve_rule_packs(&repo_root, &[config]);
        assert!(results[0].1.is_err());
    }

    #[test]
    fn a_git_source_is_not_yet_supported_and_fails_clearly() {
        let repo = tempfile::tempdir().unwrap();
        let config = RulePackConfig { id: "acme-perf".to_string(), source: RulePackSourceConfig::Git { url: "https://example.com/acme/perf-rules".to_string(), r#ref: None, subpath: None } };
        let results = resolve_rule_packs(repo.path(), &[config]);
        assert!(results[0].1.is_err());
    }
}
