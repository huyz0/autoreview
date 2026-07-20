use std::collections::{HashMap, HashSet};
use std::path::Path;

use include_dir::{include_dir, Dir};
use serde::Deserialize;

use autoreview_langsupport::Language;
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

use super::rule_pack::{run_ast_grep_scan, write_rule_file};
use crate::rule_packs::{ResolvedRulePack, MANIFEST_FILE_NAME};

/// One source of rule files to scan: the compiled-in builtin pack, or a
/// registered external pack resolved to a real directory on disk. Every
/// rule-discovery function below (`rule_categories`/`semantic_rule_ids`/
/// `rule_metadata`/`extract_pattern_rules`) walks a `&[RuleRoot]` instead
/// of the embedded tree alone — this is the actual integration point that
/// makes a registered pack's rules run exactly like builtin ones, no
/// separate execution path.
pub(crate) enum RuleRoot<'a> {
    Embedded(&'a Dir<'a>),
    Disk { id: &'a str, path: &'a Path },
}

/// Builds the full root list for a run: the embedded builtin pack first,
/// then one `Disk` root per registered, resolved pack.
pub(crate) fn rule_roots(registered_packs: &[ResolvedRulePack]) -> Vec<RuleRoot<'_>> {
    let mut roots = vec![RuleRoot::Embedded(&BUILTIN_RULES)];
    roots.extend(registered_packs.iter().map(|p| RuleRoot::Disk { id: &p.id, path: p.local_path.as_path() }));
    roots
}

/// Maps a rule pack's top-level `rules-builtin/<language>/` directory name
/// to the `Language` value(s) present in it — derived from the directory
/// layout itself, not from parsing each YAML's `language:` field, since the
/// layout already encodes it for free. One directory (`typescript`) covers
/// two grammars (`.ts` and `.tsx`), so it maps to both.
const RULE_DIR_LANGUAGES: &[(&str, &[Language])] = &[
    ("go", &[Language::Go]),
    ("java", &[Language::Java]),
    ("kotlin", &[Language::Kotlin]),
    ("javascript", &[Language::JavaScript]),
    ("typescript", &[Language::TypeScript, Language::Tsx]),
];

/// Builtin ast-grep rules, embedded at compile time for the same reason the
/// builtin skills are: a single binary shouldn't need a side-car data
/// directory installed next to it to have any deterministic coverage at all.
pub(crate) static BUILTIN_RULES: Dir = include_dir!("$CARGO_MANIFEST_DIR/rules-builtin");

fn default_category() -> String {
    "correctness".to_string()
}

fn default_kind() -> String {
    "pattern".to_string()
}

/// Semgrep-style structured metadata — entirely optional, self-documenting
/// rather than execution-affecting. Flows verbatim into
/// `AgentFinding.meta` so a rule can carry a CWE/OWASP mapping or a
/// confidence/likelihood/impact self-rating (the same fields Semgrep's own
/// registry rules use) without needing a schema change every time a new
/// piece of metadata turns out to be useful.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct RuleMetadataBlock {
    #[serde(default)]
    pub cwe: Vec<String>,
    #[serde(default)]
    pub owasp: Vec<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub likelihood: Option<String>,
    #[serde(default)]
    pub impact: Option<String>,
    #[serde(default)]
    pub subcategory: Vec<String>,
}

/// Just enough of the rule YAML shape to recover the fields common to every
/// rule *kind* (`category`, `semantic`, `kind`, `metadata`) — fields
/// `ast-grep` itself doesn't understand (verified empirically: it neither
/// errors on unrecognized top-level keys nor echoes them back in `--json`
/// scan output, so none of this can flow through ast-grep's own pipeline).
/// Parsed directly from the embedded rule files ourselves, independent of
/// the ast-grep invocation — and, since `kind` now discriminates which
/// *backend* actually executes a rule (`pattern` → this file's `ast-grep`
/// subprocess, `taint` → `crates/dataflow`'s taint engine, `threshold` →
/// `complexity.rs`), this same struct/parse is the shared foundation every
/// backend's own loader builds on, not a `pattern`-specific one.
#[derive(Debug, Deserialize)]
pub struct RuleMeta {
    pub id: String,
    #[serde(default = "default_category")]
    pub category: String,
    /// Marks a rule as a "semantic rule" candidate: syntactically precise
    /// but semantically approximate (no type resolution, no dataflow) —
    /// higher false-positive risk than a plain deterministic rule, so its
    /// findings always get a Stage 3.5 cheap-LLM confirmation pass
    /// regardless of severity/category, rather than only when its whole
    /// category happens to be marked noisy. This is the two-pass design:
    /// Stage 1 (syntactic) narrows to a specific finding + line/snippet,
    /// then only that finding (not the whole codebase) is handed to the
    /// verifier to confirm or refute.
    #[serde(default)]
    pub semantic: bool,
    /// Which backend executes this rule. `"pattern"` (the default, and
    /// every rule written before this field existed) means the `rule:`/
    /// `constraints:` block is native ast-grep syntax handed to the CLI
    /// subprocess unchanged. Any other value means this file is invisible
    /// to that subprocess (see `extract_pattern_rules`) and its body is
    /// interpreted by that kind's own backend instead.
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub metadata: Option<RuleMetadataBlock>,
}

fn parse_rule_meta(contents: &str) -> Option<RuleMeta> {
    serde_yaml::from_str(contents).ok()
}

/// Builds a `ruleId -> category` lookup by parsing every rule file across
/// `roots` for just its `id`/`category` fields. Rules that fail to parse
/// (shouldn't happen for our own builtin files, but this must never be the
/// reason a whole scan fails) are silently skipped — their findings fall
/// back to the default category via `unwrap_or`.
fn rule_categories(roots: &[RuleRoot]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    walk_rule_files(roots, &mut |meta| {
        map.insert(meta.id.clone(), meta.category.clone());
    });
    map
}

/// The set of rule ids declaring `semantic: true` across `registered_packs`
/// plus the builtin pack — see `RuleMeta::semantic`'s docs for what that
/// means and why. A registered pack's `semantic: true` rules get exactly
/// the same Stage 3.5 confirmation treatment as builtin ones, for free.
pub fn semantic_rule_ids(registered_packs: &[ResolvedRulePack]) -> std::collections::HashSet<String> {
    let roots = rule_roots(registered_packs);
    let mut set = std::collections::HashSet::new();
    walk_rule_files(&roots, &mut |meta| {
        if meta.semantic {
            set.insert(meta.id.clone());
        }
    });
    set
}

/// A `ruleId -> metadata` lookup for every rule that declares a `metadata:`
/// block, regardless of `kind` — used to populate `AgentFinding.meta`.
/// Rules with no `metadata:` block simply don't appear here (not inserted
/// as an empty entry), so callers should treat a missing key as "no
/// metadata was declared," not "metadata was declared empty."
fn rule_metadata(roots: &[RuleRoot]) -> HashMap<String, RuleMetadataBlock> {
    let mut map = HashMap::new();
    walk_rule_files(roots, &mut |meta| {
        if let Some(metadata) = &meta.metadata {
            map.insert(meta.id.clone(), metadata.clone());
        }
    });
    map
}

/// Shared walk over every rule file across `roots`, parsing each into
/// `RuleMeta` and handing it to `visit` — the one place all of
/// `rule_categories`/`semantic_rule_ids`/`rule_metadata` (and
/// `extract_pattern_rules`'s filtering) read rule sources from.
fn walk_rule_files(roots: &[RuleRoot], visit: &mut impl FnMut(&RuleMeta)) {
    walk_rule_contents(roots, &mut |contents| {
        if let Some(meta) = parse_rule_meta(contents) {
            visit(&meta);
        }
    });
}

/// Same walk, but hands back each `.yml`/`.yaml` file's raw text instead of
/// the parsed `RuleMeta` — used by non-pattern-kind loaders (e.g.
/// `taint_rules.rs`) that need to deserialize their own kind-specific body
/// (`sources:`/`sinks:`/...) from the same files, not just the common
/// fields `RuleMeta` covers.
pub(crate) fn walk_rule_contents(roots: &[RuleRoot], visit: &mut impl FnMut(&str)) {
    for root in roots {
        match root {
            RuleRoot::Embedded(dir) => walk_embedded_dir(dir, visit),
            RuleRoot::Disk { path, .. } => walk_disk_dir(path, visit),
        }
    }
}

fn walk_embedded_dir(dir: &Dir, visit: &mut impl FnMut(&str)) {
    for file in dir.files() {
        let is_yaml = file.path().extension().is_some_and(|ext| ext == "yml" || ext == "yaml");
        if !is_yaml {
            continue;
        }
        if let Some(contents) = file.contents_utf8() {
            visit(contents);
        }
    }
    for subdir in dir.dirs() {
        walk_embedded_dir(subdir, visit);
    }
}

/// Recursive `.yml`/`.yaml` walk over a registered pack's real directory —
/// the disk-root counterpart to `walk_embedded_dir`. Skips the pack's own
/// `rulepack.yaml` (its identity manifest, not a rule file) and, unlike the
/// embedded builtin tree, applies no language-subtree gating: a pack isn't
/// required to organize its rules by language directory the way
/// `rules-builtin/` does, so there's no directory-name convention to gate
/// on.
fn walk_disk_dir(path: &Path, visit: &mut impl FnMut(&str)) {
    let Ok(entries) = std::fs::read_dir(path) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            walk_disk_dir(&entry_path, visit);
            continue;
        }
        let is_yaml = entry_path.extension().is_some_and(|ext| ext == "yml" || ext == "yaml");
        if !is_yaml || entry_path.file_name().and_then(|n| n.to_str()) == Some(MANIFEST_FILE_NAME) {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(&entry_path) {
            visit(&contents);
        }
    }
}

fn metadata_to_meta_map(metadata: &RuleMetadataBlock) -> Option<HashMap<String, serde_json::Value>> {
    match serde_json::to_value(metadata).ok()? {
        serde_json::Value::Object(map) => Some(map.into_iter().collect()),
        _ => None,
    }
}

pub(crate) const SOURCE_EXTENSIONS: &[&str] = &["go", "java", "kt", "kts", "ts", "mts", "cts", "tsx", "js", "jsx", "mjs", "cjs"];

pub(crate) fn is_relevant_source_file(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).map(|ext| SOURCE_EXTENSIONS.contains(&ext)).unwrap_or(false)
}

pub(crate) fn map_severity(sg_severity: &str) -> Severity {
    match sg_severity {
        "error" => Severity::High,
        "warning" => Severity::Medium,
        "info" => Severity::Low,
        "hint" => Severity::Info,
        _ => Severity::Medium,
    }
}

pub(crate) fn title_from_rule_id(rule_id: &str) -> String {
    rule_id
        .split(['-', '_'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs the ast-grep rule pack — the embedded builtin rules plus any
/// registered, resolved rule packs — against the given changed files and
/// normalizes matches into analyzer findings. Returns an empty list (not an
/// error) if none of the changed files match a language we ship rules for,
/// or if the `ast-grep` binary isn't on PATH — Stage 1 is meant to degrade
/// gracefully, not block the rest of the review.
pub fn run_ast_grep(repo_root: &Path, changed_files: &[String], registered_packs: &[ResolvedRulePack]) -> anyhow::Result<Vec<AgentFinding>> {
    // A deleted file's path still shows up in `git diff --numstat` — ast-grep
    // tolerates a missing path (skips it with a stderr warning, still scans
    // the rest), but filtering up front avoids the noise and the wasted work.
    let relevant: Vec<&str> = changed_files.iter().map(String::as_str).filter(|p| is_relevant_source_file(p) && repo_root.join(p).exists()).collect();
    if relevant.is_empty() {
        return Ok(vec![]);
    }

    // Rule-group apply condition: only extract pattern rules for languages
    // actually present in this diff — a Go-only diff never needs to see
    // the Java/Kotlin/TypeScript/JavaScript rule subtrees at all. This gate
    // only applies to the embedded builtin tree's own language-directory
    // convention — a registered pack isn't required to organize by
    // language, so its rules are never skipped by this check.
    let languages_present = autoreview_langsupport::languages_present(relevant.iter().copied());

    let temp_dir = tempfile::tempdir()?;
    let rules_dir = temp_dir.path().join("rules");
    std::fs::create_dir_all(&rules_dir)?;
    let roots = rule_roots(registered_packs);
    let written = extract_pattern_rules(&roots, &rules_dir, &languages_present)?;
    // Defensive, not expected to trigger today (SOURCE_EXTENSIONS and
    // RULE_DIR_LANGUAGES are in lockstep) — future-proofs against the two
    // lists drifting apart, e.g. a new source extension added to one and
    // not the other.
    if written == 0 {
        return Ok(vec![]);
    }

    let matches = match run_ast_grep_scan(temp_dir.path(), repo_root, &relevant)? {
        Some(matches) => matches,
        None => return Ok(vec![]),
    };

    let categories = rule_categories(&roots);
    let metadata = rule_metadata(&roots);
    let pack_provenance = pack_rule_provenance(registered_packs);
    Ok(matches.iter().filter_map(|m| match_to_finding(m, &categories, &metadata, &pack_provenance)).collect())
}

/// Maps each registered pack's own declared rule ids back to the pack's
/// id — stamped into `AgentFinding.meta["rulePackId"]` so a pack-sourced
/// finding is identifiable in a report. Worth doing precisely because
/// packs run "full trust immediately" (no shadow-mode staging gate that
/// would otherwise visually set them apart from builtin findings).
/// Pattern-kind rules only for now — taint/threshold provenance isn't
/// wired yet, a deliberate scope cut, not an oversight.
fn pack_rule_provenance(registered_packs: &[ResolvedRulePack]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pack in registered_packs {
        walk_disk_dir(&pack.local_path, &mut |contents| {
            if let Some(meta) = parse_rule_meta(contents) {
                map.insert(meta.id, pack.id.clone());
            }
        });
    }
    map
}

/// Copies every `kind: pattern` (or `kind`-absent) rule file across `roots`
/// into `dest`. ast-grep's CLI errors if any file under its `ruleDirs`
/// lacks a valid `rule:` key, so a `kind: taint`/`kind: threshold` file
/// (which has no `rule:` block at all — its body is
/// `sources:`/`sinks:`/`metric:`/`threshold:` instead) must never reach the
/// subprocess. This is what makes "one rules directory [per root], three
/// execution backends" possible: every rule lives in the same source tree
/// (`rules-builtin/<lang>/<category>/<id>.yml`, or a pack's own layout),
/// but only the pattern-kind ones are ever visible to `ast-grep scan`.
/// Returns the number of files actually written, across all roots.
fn extract_pattern_rules(roots: &[RuleRoot], dest: &Path, languages_present: &HashSet<Language>) -> anyhow::Result<usize> {
    let mut written = 0usize;
    for root in roots {
        written += match root {
            RuleRoot::Embedded(dir) => extract_pattern_rules_embedded(dir, dest, languages_present)?,
            RuleRoot::Disk { id, path } => extract_pattern_rules_disk(id, path, dest)?,
        };
    }
    Ok(written)
}

/// Only for a language subdirectory that has at least one representative
/// in `languages_present` — the rule-group apply condition, gating whole
/// language subtrees before any per-file work happens. Specific to the
/// embedded builtin tree's own `<language>/<category>/<id>.yml` directory
/// convention.
fn extract_pattern_rules_embedded(dir: &Dir, dest: &Path, languages_present: &HashSet<Language>) -> anyhow::Result<usize> {
    let mut written = 0usize;
    for file in dir.files() {
        let is_yaml = file.path().extension().is_some_and(|ext| ext == "yml" || ext == "yaml");
        if !is_yaml {
            continue;
        }
        let Some(contents) = file.contents_utf8() else { continue };
        let kind = parse_rule_meta(contents).map(|m| m.kind).unwrap_or_else(default_kind);
        if kind != "pattern" {
            continue;
        }
        write_rule_file(&dest.join(file.path()), contents)?;
        written += 1;
    }
    for subdir in dir.dirs() {
        if let Some(dir_name) = subdir.path().file_name().and_then(|n| n.to_str()) {
            if let Some((_, langs)) = RULE_DIR_LANGUAGES.iter().find(|(name, _)| *name == dir_name) {
                if !langs.iter().any(|l| languages_present.contains(l)) {
                    continue;
                }
            }
        }
        written += extract_pattern_rules_embedded(subdir, dest, languages_present)?;
    }
    Ok(written)
}

/// Disk-root counterpart, no language-subtree gating (see `walk_disk_dir`'s
/// docs for why) — writes under `dest/packs/<pack_id>/<relative path>` so
/// two packs (or a pack and the builtin tree) can never collide on the
/// same relative filename inside the shared tempdir.
fn extract_pattern_rules_disk(pack_id: &str, pack_root: &Path, dest: &Path) -> anyhow::Result<usize> {
    let mut written = 0usize;
    extract_pattern_rules_disk_inner(pack_root, pack_root, pack_id, dest, &mut written)?;
    Ok(written)
}

fn extract_pattern_rules_disk_inner(current: &Path, pack_root: &Path, pack_id: &str, dest: &Path, written: &mut usize) -> anyhow::Result<()> {
    let Ok(entries) = std::fs::read_dir(current) else { return Ok(()) };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            extract_pattern_rules_disk_inner(&path, pack_root, pack_id, dest, written)?;
            continue;
        }
        let is_yaml = path.extension().is_some_and(|ext| ext == "yml" || ext == "yaml");
        if !is_yaml || path.file_name().and_then(|n| n.to_str()) == Some(MANIFEST_FILE_NAME) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else { continue };
        let kind = parse_rule_meta(&contents).map(|m| m.kind).unwrap_or_else(default_kind);
        if kind != "pattern" {
            continue;
        }
        let relative = path.strip_prefix(pack_root).unwrap_or(&path);
        write_rule_file(&dest.join("packs").join(pack_id).join(relative), &contents)?;
        *written += 1;
    }
    Ok(())
}

fn match_to_finding(m: &serde_json::Value, categories: &HashMap<String, String>, metadata: &HashMap<String, RuleMetadataBlock>, pack_provenance: &HashMap<String, String>) -> Option<AgentFinding> {
    let rule_id = m.get("ruleId")?.as_str()?.to_string();
    let file = m.get("file")?.as_str()?.to_string();
    let message = m.get("message").and_then(|v| v.as_str()).unwrap_or("(no message provided by rule)").to_string();
    let severity = map_severity(m.get("severity").and_then(|v| v.as_str()).unwrap_or("warning"));
    let snippet = m.get("lines").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let category = categories.get(&rule_id).cloned().unwrap_or_else(default_category);
    let mut meta = metadata.get(&rule_id).and_then(metadata_to_meta_map);
    if let Some(pack_id) = pack_provenance.get(&rule_id) {
        meta.get_or_insert_with(HashMap::new).insert("rulePackId".to_string(), serde_json::Value::String(pack_id.clone()));
    }

    let range = m.get("range")?;
    // ast-grep reports 0-indexed line/column; our schema is 1-indexed to
    // match how editors and `git diff` line numbers read.
    let start_line = range.get("start")?.get("line")?.as_u64()? as u32 + 1;
    let start_col = range.get("start")?.get("column").and_then(|v| v.as_u64()).map(|c| c as u32 + 1);
    let end_line = range.get("end")?.get("line")?.as_u64()? as u32 + 1;
    let end_col = range.get("end")?.get("column").and_then(|v| v.as_u64()).map(|c| c as u32 + 1);

    Some(AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".to_string(), rule_id: Some(rule_id.clone()), aspect: None, backend: None },
        category,
        severity,
        confidence: 1.0,
        title: title_from_rule_id(&rule_id),
        message,
        location: Location { path: file, range: LocationRange { start_line, start_col, end_line: Some(end_line), end_col }, snippet, side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta,
        suggested_patch: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn ast_grep_available() -> bool {
        StdCommand::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn write_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, contents) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
        dir
    }

    #[test]
    fn returns_empty_without_invoking_when_no_relevant_files() {
        // This must short-circuit before spawning ast-grep at all, so it's a
        // meaningful test even in environments without the binary installed.
        let result = run_ast_grep(Path::new("/nonexistent"), &["README.md".to_string(), "package.json".to_string()], &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn returns_empty_when_ast_grep_binary_is_missing() {
        // We can't easily unset PATH here without affecting other tests in
        // this process, but ENOENT handling is exercised for real whenever
        // this suite runs in an environment without ast-grep installed.
        if ast_grep_available() {
            return;
        }
        let dir = write_repo(&[("main.go", "package main\nfunc main() {}\n")]);
        let result = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn filters_out_a_deleted_file_and_still_finds_bugs_in_the_rest() {
        // A deleted file's path still shows up in `git diff --numstat`; make
        // sure a nonexistent path doesn't stop real files from being scanned.
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\nfunc main() {\n\tif true == true {\n\t}\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string(), "deleted.go".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-no-self-comparison"));
    }

    #[test]
    fn finds_a_real_self_comparison_bug_in_go() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\nfunc main() {\n\tif true == true {\n\t}\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();

        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.source.rule_id.as_deref(), Some("go-no-self-comparison"));
        assert_eq!(finding.source.tool, "ast-grep");
        assert_eq!(finding.confidence, 1.0);
        assert_eq!(finding.category, "correctness");
        assert_eq!(finding.location.path, "main.go");
        assert_eq!(finding.location.range.start_line, 4); // 1-indexed: line 4 is "if true == true {"
    }

    #[test]
    fn extract_pattern_rules_for_go_only_copies_no_other_language_subtree() {
        let dest = tempfile::tempdir().unwrap();
        let languages_present: HashSet<Language> = [Language::Go].into_iter().collect();
        let written = extract_pattern_rules(&[RuleRoot::Embedded(&BUILTIN_RULES)], dest.path(), &languages_present).unwrap();
        assert!(written > 0, "expected at least one Go pattern rule to be written");
        for other in ["java", "kotlin", "typescript", "javascript"] {
            assert!(!dest.path().join(other).exists(), "expected no {other} subtree to be copied for a Go-only diff");
        }
        assert!(dest.path().join("go").exists(), "expected the go subtree to be copied");
    }

    #[test]
    fn extract_pattern_rules_for_no_languages_writes_nothing() {
        let dest = tempfile::tempdir().unwrap();
        let written = extract_pattern_rules(&[RuleRoot::Embedded(&BUILTIN_RULES)], dest.path(), &HashSet::new()).unwrap();
        assert_eq!(written, 0);
    }

    #[test]
    fn finds_a_gob_decode_of_untrusted_input_in_go() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "main.go",
            "package main\n\nimport \"encoding/gob\"\n\nfunc handle(body []byte) {\n\tvar v MyType\n\tgob.NewDecoder(body).Decode(&v)\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-insecure-deserialization"));
        assert_eq!(findings[0].category, "security");
    }

    #[test]
    fn finds_unreachable_code_after_a_return_in_go() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\nfunc f(x int) int {\n\treturn x\n\tprintln(\"unreachable\")\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-unreachable-code"));
    }

    #[test]
    fn does_not_flag_go_code_after_a_return_inside_an_if_block() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\nfunc f(x int) int {\n\tif x > 0 {\n\t\treturn x\n\t}\n\treturn 0\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-unreachable-code")));
    }

    #[test]
    fn finds_a_named_empty_interface_in_go() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\ntype Marker interface {\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-empty-interface"));
        assert_eq!(findings[0].category, "design");
    }

    #[test]
    fn does_not_flag_a_go_interface_with_methods() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\ntype Reader interface {\n\tRead(p []byte) (int, error)\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-empty-interface")));
    }

    #[test]
    fn finds_a_go_test_with_no_assertions() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main_test.go", "package main\n\nimport \"testing\"\n\nfunc TestNothing(t *testing.T) {\n\tx := doSomething()\n\t_ = x\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main_test.go".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-test-without-assertions"));
    }

    #[test]
    fn does_not_flag_a_go_test_with_a_real_assertion() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "main_test.go",
            "package main\n\nimport \"testing\"\n\nfunc TestGood(t *testing.T) {\n\tif got := doSomething(); got != 5 {\n\t\tt.Errorf(\"got %d\", got)\n\t}\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["main_test.go".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-test-without-assertions")));
    }

    #[test]
    fn does_not_flag_a_non_test_function_named_test_something() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\nfunc TestingHelperNotATest() int {\n\treturn 1\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-test-without-assertions")));
    }

    #[test]
    fn finds_unreachable_code_after_a_throw_in_java() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.java", "public class Foo {\n    void g() {\n        throw new RuntimeException(\"boom\");\n        System.out.println(\"dead\");\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.java".to_string()], &[]).unwrap();
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("java-unreachable-code")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_java_code_after_a_return_inside_an_if_block() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.java", "public class Foo {\n    int k(int x) {\n        if (x > 0) {\n            return x;\n        }\n        return 0;\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.java".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("java-unreachable-code")));
    }

    #[test]
    fn finds_unreachable_code_after_exitprocess_in_kotlin() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.kt", "class Foo {\n    fun h(x: Int) {\n        exitProcess(1)\n        println(\"dead2\")\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.kt".to_string()], &[]).unwrap();
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("kotlin-unreachable-code")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_kotlin_code_after_a_return_inside_an_if_block() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.kt", "class Foo {\n    fun k(x: Int): Int {\n        if (x > 0) {\n            return x\n        }\n        return 0\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.kt".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("kotlin-unreachable-code")));
    }

    #[test]
    fn finds_a_java_test_with_no_assertions() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "FooTest.java",
            "import org.junit.Test;\n\npublic class FooTest {\n    @Test\n    public void testNothing() {\n        int x = doSomething();\n    }\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["FooTest.java".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("java-test-without-assertions"));
    }

    #[test]
    fn does_not_flag_a_java_test_with_a_real_assertion() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "FooTest.java",
            "import org.junit.Test;\nimport static org.junit.Assert.assertEquals;\n\npublic class FooTest {\n    @Test\n    public void testGood() {\n        assertEquals(5, doSomething());\n    }\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["FooTest.java".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("java-test-without-assertions")));
    }

    #[test]
    fn finds_a_kotlin_test_with_no_assertions() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "FooTest.kt",
            "import org.junit.Test\n\nclass FooTest {\n    @Test\n    fun testNothing() {\n        val x = doSomething()\n    }\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["FooTest.kt".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("kotlin-test-without-assertions"));
    }

    #[test]
    fn does_not_flag_a_kotlin_test_with_a_real_assertion() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "FooTest.kt",
            "import org.junit.Test\nimport org.junit.Assert.assertEquals\n\nclass FooTest {\n    @Test\n    fun testGood() {\n        assertEquals(5, doSomething())\n    }\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["FooTest.kt".to_string()], &[]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("kotlin-test-without-assertions")));
    }

    #[test]
    fn finds_a_real_empty_error_check_in_go() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "main.go",
            "package main\n\nfunc doIt() error { return nil }\n\nfunc main() {\n\tif err := doIt(); err != nil {\n\t}\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-empty-error-check"));
    }

    #[test]
    fn does_not_flag_properly_handled_errors() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "main.go",
            "package main\n\nimport \"fmt\"\n\nfunc doIt() error { return nil }\n\nfunc main() {\n\tif err := doIt(); err != nil {\n\t\tfmt.Println(err)\n\t}\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn finds_a_real_bug_in_java() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "Sample.java",
            "public class Sample {\n    void run() {\n        try {\n            doThing();\n        } catch (Exception e) {\n        }\n    }\n    void doThing() {}\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["Sample.java".to_string()], &[]).unwrap();
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("java-empty-catch-block")), "got: {findings:#?}");
    }

    #[test]
    fn finds_a_real_bug_in_kotlin() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Sample.kt", "fun main() {\n    val s: String? = null\n    println(s!!.length)\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Sample.kt".to_string()], &[]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("kotlin-avoid-not-null-assertion"));
    }

    #[test]
    fn title_from_rule_id_is_human_readable() {
        assert_eq!(title_from_rule_id("go-no-self-comparison"), "Go No Self Comparison");
    }

    #[test]
    fn rule_categories_reads_the_declared_category_for_every_builtin_rule() {
        let categories = rule_categories(&[RuleRoot::Embedded(&BUILTIN_RULES)]);
        assert_eq!(categories.get("go-no-self-comparison").map(String::as_str), Some("correctness"));
        assert_eq!(categories.get("kotlin-avoid-not-null-assertion").map(String::as_str), Some("correctness"));
        assert_eq!(categories.get("go-hardcoded-credential").map(String::as_str), Some("security"));
        assert!(categories.len() >= 33, "expected at least the 6 correctness + 27 security builtin rules to declare a category, got {}: {categories:?}", categories.len());
    }

    #[test]
    fn parse_rule_meta_defaults_kind_to_pattern_when_absent() {
        let meta = parse_rule_meta("id: some-rule\ncategory: correctness\n").unwrap();
        assert_eq!(meta.kind, "pattern");
    }

    #[test]
    fn parse_rule_meta_reads_an_explicit_kind() {
        let meta = parse_rule_meta("id: some-taint-rule\nkind: taint\n").unwrap();
        assert_eq!(meta.kind, "taint");
    }

    #[test]
    fn parse_rule_meta_reads_a_metadata_block() {
        let meta = parse_rule_meta("id: some-rule\nmetadata:\n  cwe: [\"CWE-79\"]\n  confidence: HIGH\n").unwrap();
        let metadata = meta.metadata.expect("metadata block should parse");
        assert_eq!(metadata.cwe, vec!["CWE-79".to_string()]);
        assert_eq!(metadata.confidence.as_deref(), Some("HIGH"));
    }

    #[test]
    fn rule_metadata_only_contains_rules_that_declare_a_metadata_block() {
        let metadata = rule_metadata(&[RuleRoot::Embedded(&BUILTIN_RULES)]);
        let weak_hash = metadata.get("go-weak-hash").expect("go-weak-hash should declare metadata");
        assert_eq!(weak_hash.cwe, vec!["CWE-327".to_string()]);
        assert!(!metadata.contains_key("go-no-self-comparison"), "a rule with no metadata: block must not appear");
    }

    #[test]
    fn metadata_flows_through_into_a_real_finding_via_the_meta_field() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\nimport \"crypto/md5\"\n\nfunc f() {\n\tmd5.New()\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()], &[]).unwrap();
        let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some("go-weak-hash")).expect("go-weak-hash should fire");
        let meta = finding.meta.as_ref().expect("go-weak-hash declares a metadata: block, meta should be Some");
        assert_eq!(meta.get("cwe").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|v| v.as_str()), Some("CWE-327"));
    }

    #[test]
    fn semantic_rule_ids_reads_rules_declaring_semantic_true() {
        let ids = semantic_rule_ids(&[]);
        assert!(ids.contains("go-nested-loop-linear-search"));
        assert!(ids.contains("java-object-instantiation-in-loop"));
        assert!(ids.contains("go-unclosed-http-response-body"));
        assert!(!ids.contains("go-no-self-comparison"), "a plain rule with no semantic: true field must not be included");
    }

    #[test]
    fn a_rule_with_no_declared_category_falls_back_to_correctness() {
        let categories: HashMap<String, String> = HashMap::new();
        let metadata: HashMap<String, RuleMetadataBlock> = HashMap::new();
        let m = serde_json::json!({
            "ruleId": "some-undeclared-rule",
            "file": "a.go",
            "message": "m",
            "severity": "warning",
            "lines": "x",
            "range": {"start": {"line": 0, "column": 0}, "end": {"line": 0, "column": 1}}
        });
        let finding = match_to_finding(&m, &categories, &metadata, &HashMap::new()).unwrap();
        assert_eq!(finding.category, "correctness");
    }

    #[test]
    fn a_pack_sourced_rule_gets_its_pack_id_stamped_into_meta() {
        let categories: HashMap<String, String> = HashMap::new();
        let metadata: HashMap<String, RuleMetadataBlock> = HashMap::new();
        let pack_provenance: HashMap<String, String> = [("acme-no-println".to_string(), "acme-security".to_string())].into_iter().collect();
        let m = serde_json::json!({
            "ruleId": "acme-no-println",
            "file": "a.go",
            "message": "m",
            "severity": "warning",
            "lines": "x",
            "range": {"start": {"line": 0, "column": 0}, "end": {"line": 0, "column": 1}}
        });
        let finding = match_to_finding(&m, &categories, &metadata, &pack_provenance).unwrap();
        let meta = finding.meta.expect("expected meta to be populated with rulePackId");
        assert_eq!(meta.get("rulePackId").and_then(|v| v.as_str()), Some("acme-security"));
    }

    #[test]
    fn a_builtin_rule_gets_no_rule_pack_id_in_meta() {
        let categories: HashMap<String, String> = HashMap::new();
        let metadata: HashMap<String, RuleMetadataBlock> = HashMap::new();
        let pack_provenance: HashMap<String, String> = [("some-other-pack-rule".to_string(), "some-pack".to_string())].into_iter().collect();
        let m = serde_json::json!({
            "ruleId": "go-no-self-comparison",
            "file": "a.go",
            "message": "m",
            "severity": "warning",
            "lines": "x",
            "range": {"start": {"line": 0, "column": 0}, "end": {"line": 0, "column": 1}}
        });
        let finding = match_to_finding(&m, &categories, &metadata, &pack_provenance).unwrap();
        assert!(finding.meta.is_none());
    }
}
