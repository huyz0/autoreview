use std::collections::HashMap;
use std::path::Path;

use include_dir::Dir;
use serde::Deserialize;

use super::{rule_roots, RuleRoot};
use crate::rule_packs::{ResolvedRulePack, MANIFEST_FILE_NAME};

pub(super) fn default_category() -> String {
    "correctness".to_string()
}

pub(super) fn default_kind() -> String {
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
/// Autoreview reads from the original YAML, independent of
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

pub(super) fn parse_rule_meta(contents: &str) -> Option<RuleMeta> {
    serde_yaml::from_str(contents).ok()
}

/// Builds a `ruleId -> category` lookup by parsing every rule file across
/// `roots` for just its `id`/`category` fields. Rules that fail to parse
/// (shouldn't happen for our own builtin files, but this must never be the
/// reason a whole scan fails) are silently skipped — their findings fall
/// back to the default category via `unwrap_or`.
pub(super) fn rule_categories(roots: &[RuleRoot]) -> HashMap<String, String> {
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
pub fn semantic_rule_ids(
    registered_packs: &[ResolvedRulePack],
) -> std::collections::HashSet<String> {
    let roots = rule_roots(registered_packs);
    let mut set = std::collections::HashSet::new();
    walk_rule_files(&roots, &mut |meta| {
        if meta.semantic {
            set.insert(meta.id.clone());
        }
    });
    set.extend(super::super::framework_checks::semantic_ids());
    set
}

/// The exact rule file backing a given rule id, resolved back to its raw
/// YAML text — `autoreview explain`'s ground truth for what a rule-sourced
/// finding actually checked, rather than just the finding's own free-text
/// message. Searched in the same precedence order `rule_roots` builds:
/// builtin first, then each registered pack.
#[derive(Debug, Clone)]
pub struct RuleDefinition {
    pub id: String,
    /// `"builtin"` or the id of the registered pack the rule came from.
    pub source_label: String,
    pub kind: String,
    pub category: String,
    pub semantic: bool,
    pub yaml: String,
}

pub fn find_rule_definition(
    rule_id: &str,
    registered_packs: &[ResolvedRulePack],
) -> Option<RuleDefinition> {
    if let Some(definition) = super::super::framework_checks::definition(rule_id) {
        return Some(definition);
    }
    for root in rule_roots(registered_packs) {
        let source_label = match &root {
            RuleRoot::Embedded(_) => "builtin".to_string(),
            RuleRoot::Disk { id, .. } => id.to_string(),
        };
        let mut found = None;
        walk_rule_contents(std::slice::from_ref(&root), &mut |contents| {
            if found.is_some() {
                return;
            }
            if let Some(meta) = parse_rule_meta(contents) {
                if meta.id == rule_id {
                    found = Some(RuleDefinition {
                        id: meta.id,
                        source_label: source_label.clone(),
                        kind: meta.kind,
                        category: meta.category,
                        semantic: meta.semantic,
                        yaml: contents.to_string(),
                    });
                }
            }
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

/// A `ruleId -> metadata` lookup for every rule that declares a `metadata:`
/// block, regardless of `kind` — used to populate `AgentFinding.meta`.
/// Rules with no `metadata:` block simply don't appear here (not inserted
/// as an empty entry), so callers should treat a missing key as "no
/// metadata was declared," not "metadata was declared empty."
pub(super) fn rule_metadata(roots: &[RuleRoot]) -> HashMap<String, RuleMetadataBlock> {
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
        let is_yaml = file
            .path()
            .extension()
            .is_some_and(|ext| ext == "yml" || ext == "yaml");
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
pub(super) fn walk_disk_dir(path: &Path, visit: &mut impl FnMut(&str)) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            walk_disk_dir(&entry_path, visit);
            continue;
        }
        let is_yaml = entry_path
            .extension()
            .is_some_and(|ext| ext == "yml" || ext == "yaml");
        if !is_yaml || entry_path.file_name().and_then(|n| n.to_str()) == Some(MANIFEST_FILE_NAME) {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(&entry_path) {
            visit(&contents);
        }
    }
}

/// `pub(crate)` so the dataflow backend can build the same `meta` shape
/// for taint/call-sequence findings — a rule's CWE mapping should reach
/// the report identically regardless of which engine matched it.
pub(crate) fn metadata_to_meta_map(
    metadata: &RuleMetadataBlock,
) -> Option<HashMap<String, serde_json::Value>> {
    match serde_json::to_value(metadata).ok()? {
        serde_json::Value::Object(map) => Some(map.into_iter().collect()),
        _ => None,
    }
}
