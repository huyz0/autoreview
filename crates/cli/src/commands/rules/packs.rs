use anyhow::Context;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
struct RuleKindCounts {
    pattern: usize,
    taint: usize,
    threshold: usize,
    call_sequence: usize,
}

#[derive(Deserialize)]
struct MinimalRuleMeta {
    #[serde(default = "default_rule_kind")]
    kind: String,
}

fn default_rule_kind() -> String {
    "pattern".to_string()
}

impl RuleKindCounts {
    fn record(&mut self, kind: &str) {
        match kind {
            "taint" => self.taint += 1,
            "threshold" => self.threshold += 1,
            "call-sequence" => self.call_sequence += 1,
            _ => self.pattern += 1,
        }
    }
}

fn visit_rule_files(dir: &Path, visit: &mut impl FnMut(&Path, anyhow::Result<String>)) {
    let entries = match std::fs::read_dir(dir).context("could not read directory") {
        Ok(entries) => entries,
        Err(error) => {
            visit(dir, Err(error));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_rule_files(&path, visit);
        } else if is_rule_file(&path) {
            visit(&path, std::fs::read_to_string(&path).context("could not read file"));
        }
    }
}

fn is_rule_file(path: &Path) -> bool {
    matches!(path.extension().and_then(|ext| ext.to_str()), Some("yml" | "yaml"))
        && path.file_name().is_some_and(|name| name != "rulepack.yaml")
}

fn count_rule_kinds(pack_root: &Path) -> RuleKindCounts {
    let mut counts = RuleKindCounts::default();
    visit_rule_files(pack_root, &mut |_, contents| {
        let meta = contents.ok().and_then(|text| serde_yaml::from_str::<MinimalRuleMeta>(&text).ok());
        if let Some(meta) = meta {
            counts.record(&meta.kind);
        }
    });
    counts
}

/// Lists every pack registered in `.autoreview/rulepacks.yaml`: id, source
/// resolution status, and a rule-kind breakdown for packs that resolved
/// successfully. A pack that failed to resolve is listed with its error —
/// same information `diff`'s own `[warn]` line shows, surfaced here on
/// demand instead of only in review output.
pub fn run_rules_packs(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let config_path = autoreview_core::rule_packs_config_path(repo_root);
    let configured = autoreview_core::load_rule_packs_config(&config_path)?;
    if configured.is_empty() {
        println!("No rule packs registered ({} not found or empty).", config_path.display());
        return Ok(());
    }

    let cache_root = autoreview_core::default_rule_packs_cache_root();
    println!("Registered rule packs:\n");
    for (id, result) in autoreview_core::resolve_rule_packs(repo_root, &cache_root, &configured) {
        match result {
            Ok(resolved) => {
                let counts = count_rule_kinds(&resolved.local_path);
                let trust = match resolved.trust {
                    autoreview_schema::RulePackTrust::Full => "full",
                    autoreview_schema::RulePackTrust::Shadow => "shadow",
                };
                println!(
                    "  {id} [{trust}] — {} pattern, {} taint, {} threshold, {} call-sequence rule(s) ({})",
                    counts.pattern,
                    counts.taint,
                    counts.threshold,
                    counts.call_sequence,
                    resolved.local_path.display()
                );
            }
            Err(err) => println!("  {id} — failed to resolve: {err}"),
        }
    }
    Ok(())
}

/// Recognizes a `kind: git` source by shape — anything else is treated as
/// a `kind: local` filesystem path. Deliberately simple (no network probe,
/// no `git ls-remote` check): the same shorthand `git clone` itself
/// accepts, so a user who'd type it into `git clone <source>` can type the
/// same thing here.
fn looks_like_git_source(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://") || source.starts_with("git@") || source.ends_with(".git")
}

/// `autoreview rules packs add <source>` — registers a new external rule
/// pack in `.autoreview/rulepacks.yaml`. `source` is either a local
/// filesystem path (relative to `repo_root`) or a git URL/shorthand
/// (`looks_like_git_source`); the pack's own id comes from its
/// `rulepack.yaml`, not from an extra flag — resolving the source is what
/// proves it's a real, valid pack before anything gets written.
pub fn run_rules_packs_add(repo_root: &std::path::Path, source: &str) -> anyhow::Result<()> {
    let source_config = if looks_like_git_source(source) {
        autoreview_schema::RulePackSourceConfig::Git { url: source.to_string(), r#ref: None, subpath: None }
    } else {
        autoreview_schema::RulePackSourceConfig::Local { path: source.to_string() }
    };

    let cache_root = autoreview_core::default_rule_packs_cache_root();
    let (id, local_path) = autoreview_core::discover_pack_source(repo_root, &cache_root, &source_config)?;

    let config_path = autoreview_core::rule_packs_config_path(repo_root);
    let mut configured = autoreview_core::load_rule_packs_config(&config_path)?;
    if let Some(existing) = configured.iter().find(|p| p.id == id) {
        anyhow::bail!("a pack with id '{id}' is already registered (source: {:?}) — remove it from {} first if you meant to replace it", existing.source, config_path.display());
    }

    configured.push(autoreview_schema::RulePackConfig { id: id.clone(), source: source_config, trust: autoreview_schema::RulePackTrust::Full });
    autoreview_core::save_rule_packs_config(&config_path, &autoreview_schema::RulePacksFile { packs: configured })?;

    println!("Registered rule pack '{id}' (resolved to {}).", local_path.display());
    println!("Wrote {}. It runs at full trust by default — edit `trust: shadow` there to stage it first.", config_path.display());
    Ok(())
}

// Presence-only checks: successful deserialization is the signal these two
// exist and have the right shape, so the fields themselves are never read.
#[derive(Deserialize)]
#[allow(dead_code)]
struct MinimalTaintRule {
    sinks: Vec<serde_yaml::Value>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct MinimalThresholdRule {
    metric: String,
    threshold: serde_yaml::Value,
}

/// Structurally checks one rule file matches the shape its own declared
/// `kind:` needs to actually load at runtime — deliberately checking the
/// *minimum* required keys (the same ones each real loader in
/// `autoreview-core` requires), not re-implementing full rule semantics
/// here. Returns `Err` with a message naming what's missing/malformed.
pub(in crate::commands) fn validate_rule_file(contents: &str) -> anyhow::Result<(String, String)> {
    let meta: MinimalRuleMeta = serde_yaml::from_str(contents).map_err(|err| anyhow::anyhow!("not a valid rule file: {err}"))?;
    #[derive(Deserialize)]
    struct IdOnly {
        id: String,
    }
    let id_only: IdOnly = serde_yaml::from_str(contents).map_err(|_| anyhow::anyhow!("missing or empty `id` field"))?;
    if id_only.id.trim().is_empty() {
        anyhow::bail!("`id` field is empty");
    }
    match meta.kind.as_str() {
        "taint" => {
            serde_yaml::from_str::<MinimalTaintRule>(contents).map_err(|err| anyhow::anyhow!("kind: taint rule is missing a valid `sinks:` list: {err}"))?;
        }
        "threshold" => {
            serde_yaml::from_str::<MinimalThresholdRule>(contents).map_err(|err| anyhow::anyhow!("kind: threshold rule is missing `metric:`/`threshold:`: {err}"))?;
        }
        "pattern" => {
            let value: serde_yaml::Value = serde_yaml::from_str(contents)?;
            if value.get("rule").is_none() {
                anyhow::bail!("kind: pattern rule is missing a `rule:` block — ast-grep will reject it at scan time");
            }
        }
        "call-sequence" => autoreview_core::analyzers::call_order_rules::validate_call_sequence(contents)?,
        other => anyhow::bail!("unknown `kind: {other}` — expected pattern, taint, threshold, or call-sequence"),
    }
    Ok((id_only.id, meta.kind))
}

#[derive(Default)]
struct PackValidation {
    errors: Vec<String>,
    seen_ids: std::collections::HashMap<String, PathBuf>,
    counts: RuleKindCounts,
}

impl PackValidation {
    fn check(&mut self, path: &Path, contents: anyhow::Result<String>) {
        match contents.and_then(|contents| validate_rule_file(&contents)) {
            Ok((id, kind)) => {
                self.counts.record(&kind);
                if let Some(existing) = self.seen_ids.insert(id.clone(), path.to_owned()) {
                    self.errors.push(format!("{}: duplicate id '{id}' (already declared in {})", path.display(), existing.display()));
                }
            }
            Err(error) => self.errors.push(format!("{}: {error}", path.display())),
        }
    }
}

/// `autoreview rules packs validate <path>` — checks a rule pack directory
/// (not necessarily registered anywhere yet) is well-formed before its
/// author publishes it or a consumer runs `rules packs add` against it:
/// a readable `rulepack.yaml`, and every rule file structurally valid for
/// its own declared `kind`, with no duplicate ids within the pack. Doesn't
/// touch `.autoreview/rulepacks.yaml` at all — purely a local check.
pub fn run_rules_packs_validate(pack_path: &std::path::Path) -> anyhow::Result<()> {
    if !pack_path.is_dir() {
        anyhow::bail!("'{}' is not a directory", pack_path.display());
    }
    let manifest_path = pack_path.join("rulepack.yaml");
    let manifest_contents = std::fs::read_to_string(&manifest_path).map_err(|err| anyhow::anyhow!("missing or unreadable {}: {err}", manifest_path.display()))?;
    let manifest: autoreview_schema::RulePackManifest = serde_yaml::from_str(&manifest_contents).map_err(|err| anyhow::anyhow!("failed to parse {}: {err}", manifest_path.display()))?;
    if manifest.id.trim().is_empty() {
        anyhow::bail!("{}: `id` field is empty", manifest_path.display());
    }

    let mut checks = PackValidation::default();
    visit_rule_files(pack_path, &mut |path, contents| checks.check(path, contents));
    let PackValidation { counts, errors, .. } = checks;

    let description = if manifest.description.is_empty() { String::new() } else { format!(" — {}", manifest.description) };
    println!("Pack '{}' (version {}){description}", manifest.id, manifest.version);
    println!("  {} pattern, {} taint, {} threshold, {} call-sequence rule(s)", counts.pattern, counts.taint, counts.threshold, counts.call_sequence);

    if errors.is_empty() {
        println!("  no problems found.");
        Ok(())
    } else {
        println!("  {} problem(s):", errors.len());
        for err in &errors {
            println!("    - {err}");
        }
        anyhow::bail!("{} validation problem(s) found in {}:\n{}", errors.len(), pack_path.display(), errors.join("\n"));
    }
}

/// `autoreview rules packs refresh` — forces a fresh fetch for every
/// registered `kind: git` pack and reports whether its resolved commit
/// changed. Local-source packs are silently skipped (nothing to refresh —
/// see `refresh_git_rule_packs`'s own docs). Errors on individual packs
/// are reported per-pack, not aborting the rest.
pub fn run_rules_packs_refresh(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let config_path = autoreview_core::rule_packs_config_path(repo_root);
    let configured = autoreview_core::load_rule_packs_config(&config_path)?;
    let git_packs: Vec<_> = configured.iter().filter(|p| matches!(p.source, autoreview_schema::RulePackSourceConfig::Git { .. })).collect();
    if git_packs.is_empty() {
        println!("No git-source rule packs registered ({} has none, or doesn't exist).", config_path.display());
        return Ok(());
    }

    let cache_root = autoreview_core::default_rule_packs_cache_root();
    for (id, result) in autoreview_core::refresh_git_rule_packs(&cache_root, &configured) {
        match result {
            Ok(refreshed) => match refreshed.before {
                None => println!("  {id}: cloned fresh at {}", short_sha(&refreshed.after)),
                Some(before) if before == refreshed.after => println!("  {id}: up to date at {}", short_sha(&refreshed.after)),
                Some(before) => println!("  {id}: updated {} -> {}", short_sha(&before), short_sha(&refreshed.after)),
            },
            Err(err) => println!("  {id}: failed to refresh: {err}"),
        }
    }
    Ok(())
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::{run_rules_packs_validate, validate_rule_file};

    fn pack_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n").unwrap();
        dir
    }

    #[test]
    fn validate_succeeds_for_a_well_formed_pack() {
        let dir = pack_fixture();
        std::fs::write(dir.path().join("no-println.yml"), "id: acme-no-println\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nrule:\n  pattern: println($$$ARGS)\n").unwrap();
        std::fs::write(
            dir.path().join("cmd-taint.yml"),
            "id: acme-cmd-taint\nkind: taint\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nsources:\n  - call: FormValue\nsinks:\n  - call: exec.Command\nsanitizers: []\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("cyclo.yml"), "id: acme-cyclo\nlanguage: Go\ncategory: correctness\nseverity: warning\nkind: threshold\nmetric: cyclomatic-complexity\nthreshold: 8\nmessage: m\n").unwrap();

        run_rules_packs_validate(dir.path()).unwrap();
    }

    #[test]
    fn validate_fails_clearly_when_rulepack_yaml_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_rules_packs_validate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("rulepack.yaml"), "got: {err}");
    }

    #[test]
    fn validate_reports_a_pattern_rule_missing_its_rule_block() {
        let dir = pack_fixture();
        std::fs::write(dir.path().join("broken.yml"), "id: acme-broken\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\n").unwrap();

        let err = run_rules_packs_validate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("1 validation problem"), "got: {err}");
    }

    #[test]
    fn validate_reports_a_taint_rule_missing_sinks() {
        let dir = pack_fixture();
        std::fs::write(dir.path().join("broken.yml"), "id: acme-broken\nkind: taint\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nsources:\n  - call: FormValue\n").unwrap();

        let err = run_rules_packs_validate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("1 validation problem"), "got: {err}");
    }

    #[test]
    fn validate_succeeds_for_a_call_sequence_rule() {
        let dir = pack_fixture();
        std::fs::write(
            dir.path().join("unreleased-lock.yml"),
            "id: acme-unreleased-lock\nkind: call-sequence\nlanguage: Java\ncategory: correctness\nseverity: error\nmessage: m\nafter:\n  - call: lock\nunless:\n  - call: unlock\ncheckBeforeReturn: true\n",
        )
        .unwrap();

        run_rules_packs_validate(dir.path()).unwrap();
    }

    /// A `kind: call-sequence` rule
    /// used to fall through their `_ => counts.pattern += 1` wildcard arm
    /// and get silently counted as a pattern rule in `rules packs`/
    /// `rules packs validate`'s printed breakdown. `validate_rule_file`'s
    /// own returned `kind` is the single source of truth for that
    /// counting, so pinning it here catches a regression at either call
    /// site without needing to capture stdout.
    #[test]
    fn validate_rule_file_returns_call_sequence_as_its_own_kind_not_pattern() {
        let contents = "id: acme-unreleased-lock\nkind: call-sequence\nlanguage: Java\ncategory: correctness\nseverity: error\nmessage: m\nafter:\n  - call: lock\nunless:\n  - call: unlock\n";
        let (id, kind) = validate_rule_file(contents).unwrap();
        assert_eq!(id, "acme-unreleased-lock");
        assert_eq!(kind, "call-sequence");
    }

    #[test]
    fn validate_reports_a_call_sequence_rule_missing_unless() {
        let dir = pack_fixture();
        std::fs::write(dir.path().join("broken.yml"), "id: acme-broken\nkind: call-sequence\nlanguage: Java\ncategory: correctness\nseverity: error\nmessage: m\nafter:\n  - call: lock\nunless: []\ncheckBeforeReturn: true\n").unwrap();

        let err = run_rules_packs_validate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("1 validation problem"), "got: {err}");
        assert!(err.to_string().contains("unless"), "got: {err}");
    }

    #[test]
    fn validate_reports_an_unknown_kind_clearly() {
        let dir = pack_fixture();
        std::fs::write(dir.path().join("broken.yml"), "id: acme-broken\nkind: made-up-kind\nlanguage: Java\ncategory: correctness\nseverity: error\nmessage: m\n").unwrap();

        let err = run_rules_packs_validate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("unknown `kind: made-up-kind`"), "got: {err}");
    }

    #[test]
    fn validate_reports_a_duplicate_id_across_two_files() {
        let dir = pack_fixture();
        std::fs::write(dir.path().join("a.yml"), "id: acme-dup\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nrule:\n  pattern: println($$$ARGS)\n").unwrap();
        std::fs::write(dir.path().join("b.yml"), "id: acme-dup\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nrule:\n  pattern: fmt.Println($$$ARGS)\n").unwrap();

        let err = run_rules_packs_validate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("duplicate id 'acme-dup'"), "got: {err}");
    }
}
