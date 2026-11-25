//! Turns `autoreview-dataflow`'s CFG-based rule checks into reportable
//! findings — same separation as `symindex_check.rs` vs `autoreview-
//! symindex`: the dataflow crate has no knowledge of the schema's
//! `Finding` type, so that mapping lives here.
//!
//! Phase 3 added Go's `append-shared-backing-array`; Phase 4 added
//! `typed-nil-interface-return`; Phase 5 adds `loopvar-capture-pre-1.22`
//! and `loopvar-address-pre-1.22` — all drop-in replacements for
//! text-heuristic versions previously in `analyzers::practices` (same
//! rule ids, so this is not new rule surface, just sounder
//! implementations). `run_practices_check`'s Go branch no longer calls
//! any of the four old heuristics for this reason.
//!
//! `typed-nil-interface-return`'s interprocedural call resolution covers
//! both same-package and cross-package calls (see `autoreview_dataflow::
//! rules::go_typed_nil_interface_return`'s module docs for the two-pass
//! design): pass 1's summaries come from `package_summaries` (every `.go`
//! file in the changed file's own directory — Go's directory=package
//! convention) merged with `imported_package_summaries` (the same scan
//! repeated for every *other* in-module package the file imports,
//! resolved via `go.mod`'s module path plus the import's own path suffix,
//! keyed by `pkgname.Func` to match a qualified call's own lowered form).
//! A call into a genuinely external (non-module) package is still an
//! unknown boundary — there's no source here to derive a summary from.
//! This deliberately doesn't go through `autoreview_symindex::SymbolIndex`
//! — its Go extractor only indexes receiver methods whose struct is
//! declared in the same file, so it can't resolve the free-function case
//! this rule needs; `package_summaries`/`imported_package_summaries` are a
//! self-contained scan instead.
//!
//! Taint rules (`go-command-injection-taint` and friends) used to be
//! hand-written Rust `TaintSpec` constants, one per rule, each with its
//! own `run_*_taint` wrapper here. They're now declarative YAML
//! (`kind: taint` in `crates/core/rules-builtin/`), loaded at runtime by
//! `taint_rules::load_taint_rules` and run generically via
//! `dsl::run_taint_rules` — adding a new taint rule for a language that
//! already has a lowering pass doesn't touch this file at all.
//!
//! Java (`lower::java`), Kotlin (`lower::kotlin`), and JavaScript/
//! TypeScript/TSX (`lower::javascript`, one lowering module shared across
//! all three) share the taint and call-sequence engines with Go.

mod cache;
mod dispatch;
mod dsl;
mod functions;
mod keyed_imports;
mod go_practices;
mod source;

pub use dsl::DataflowRuleDef;
use std::collections::HashMap;
use std::path::Path;

use tree_sitter::{Node, Tree};

use autoreview_dataflow::cfg::{Cfg, Stmt};
use autoreview_dataflow::rules::go_typed_nil_interface_return;
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

use cache::{content_hash, ParseCache, ParsedFile, PersistentSummaryCache, SummaryCaches};
use functions::{function_declarations, go_functions, java_functions};
use source::SummarySource;
use super::call_order_rules;
use super::taint_rules;
use crate::rule_packs::ResolvedRulePack;

fn make_finding(rule_id: &str, category: &str, severity: Severity, path: &str, line: u32, title: String, message: String) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-dataflow".to_string(), rule_id: Some(rule_id.to_string()), aspect: None, backend: None },
        category: category.to_string(),
        severity,
        confidence: 1.0,
        title,
        message,
        location: Location { path: path.to_string(), range: LocationRange { start_line: line, end_line: Some(line), ..Default::default() }, snippet: String::new(), side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// The `meta` map for a dataflow-engine finding: the rule's own
/// `metadata:` block (CWE/OWASP/...) plus `rulePackId` when it came from
/// a registered pack.
///
/// The metadata half was missing entirely until now — `RuleMetadataBlock`
/// documents itself as flowing "verbatim into `AgentFinding.meta`", which
/// held for pattern rules but not for taint/call-sequence ones. Four
/// builtin call-sequence rules declared CWE mappings that were parsed and
/// then dropped on the floor, so a real XXE or unreleased-lock finding
/// reached the report with no CWE at all.
fn rule_meta(pack_id: Option<&String>, metadata: Option<&super::ast_grep::RuleMetadataBlock>) -> Option<HashMap<String, serde_json::Value>> {
    let mut meta = metadata.and_then(super::ast_grep::metadata_to_meta_map).unwrap_or_default();
    if let Some(pack_id) = pack_id {
        meta.insert("rulePackId".to_string(), serde_json::Value::String(pack_id.clone()));
    }
    (!meta.is_empty()).then_some(meta)
}

/// A language's adapter into the shared cross-file summary resolver below
/// (rule engine roadmap item 2, see `RULE_ENGINE_RESEARCH.md`): the four
/// bare-name-keyed "scan these files, extract each function, lower it,
/// compute a summary" loops previously duplicated once per language
/// (Go's `scan_dir_for_pointer_summaries` and, before this, a Java branch
/// and a Kotlin branch inside `add_npe_summaries_from_file`) are now one
/// generic implementation (`scan_files_for_summaries`), parameterized by
/// how each language extracts its functions/names/CFGs — every future
/// interprocedural rule for an already-lowerable language gets same-file
/// resolution for free by supplying one of these instead of writing a
/// fourth bespoke scan loop. Plain `fn` pointers (not closures) since
/// every implementation is already a top-level function with exactly this
/// shape; a `for<'a> fn(&'a Tree) -> Vec<Node<'a>>` genuinely can't be
/// expressed as a trait object without extra indirection, and doesn't
/// need to be — these never vary at runtime, only per call site.
struct LanguageAdapter {
    language: autoreview_langsupport::Language,
    extensions: &'static [&'static str],
    extract_functions: for<'a> fn(&'a Tree) -> Vec<Node<'a>>,
    function_name: fn(Node, &[u8]) -> Option<String>,
    lower_function: fn(&[u8], Node) -> Cfg<Stmt>,
}

const GO_ADAPTER: LanguageAdapter =
    LanguageAdapter { language: autoreview_langsupport::Language::Go, extensions: &["go"], extract_functions: go_functions, function_name: autoreview_dataflow::lower::go::function_name, lower_function: autoreview_dataflow::lower::go::lower_function };

const JAVA_ADAPTER: LanguageAdapter = LanguageAdapter {
    language: autoreview_langsupport::Language::Java,
    extensions: &["java"],
    extract_functions: java_functions,
    function_name: autoreview_dataflow::lower::java::function_name,
    lower_function: autoreview_dataflow::lower::java::lower_function,
};

const KOTLIN_ADAPTER: LanguageAdapter = LanguageAdapter {
    language: autoreview_langsupport::Language::Kotlin,
    extensions: &["kt", "kts"],
    extract_functions: function_declarations,
    function_name: autoreview_dataflow::lower::kotlin::function_name,
    lower_function: autoreview_dataflow::lower::kotlin::lower_function,
};

/// The shared core every interprocedural rule's own-directory and
/// imported-files resolution calls into: scan every already-resolved
/// candidate `.{ext}` file under `dir` (filtered to `adapter`'s own
/// extensions), and for each function `should_summarize` accepts (or
/// every function, if `None` — Java/Kotlin's NPE-risk rule has no
/// declared-return-type gate the way Go's pointer-only one does),
/// `compute_summary` produces the value it's keyed by bare function name
/// under. Best-effort: a file that fails to read or parse is silently
/// skipped rather than failing the whole scan, since the current file's
/// own findings still matter even if a sibling or an imported package's
/// file can't be read.
/// `package_filter`, when `Some`, skips any candidate file whose own
/// `declared_package` (works for Go's `package main` no less than Java/
/// Kotlin's dotted form — see `autoreview_archgraph::declared_package`'s
/// implementation) doesn't match — needed for Java/Kotlin, where
/// directory != package isn't a language guarantee, and a genuine
/// correctness improvement for Go too: a directory can legally hold both
/// `package foo` and an external test package `package foo_test` side by
/// side, which the pre-generalization `scan_dir_for_pointer_summaries`
/// didn't distinguish (a `foo_test` helper could leak into `foo`'s own
/// same-package summary map). `None` is used for the cross-package/
/// imported-directory case, where the target's exact declared package
/// name isn't reliably known in advance (only the import path's last
/// segment is — see `imported_package_summaries`'s own doc comment on
/// why that's a heuristic, not a guarantee).
// `compute_summary` takes `&dyn Fn` rather than a plain `fn` pointer —
// widened for rule engine roadmap item 6 (interprocedural taint), where
// each taint rule needs its *own* summary closure capturing that rule's
// own `TaintSpec` (`|cfg| taint::compute_source_summary(&spec, cfg)`),
// which a bare `fn` pointer structurally can't express. A plain `fn`
// item like `go_typed_nil_interface_return::compute_summary` still
// works at every existing call site unchanged — `&some_fn_item` coerces
// to `&dyn Fn` automatically.
fn scan_dir_for_summaries(
    dir: &Path,
    adapter: &LanguageAdapter,
    package_filter: Option<&str>,
    should_summarize: Option<fn(Node) -> bool>,
    compute_summary: &dyn Fn(&Cfg<Stmt>) -> bool,
    rule_id: &str,
    caches: &SummaryCaches,
) -> HashMap<String, bool> {
    let mut summaries = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return summaries };
    for entry in entries.filter_map(Result::ok) {
        let candidate_path = entry.path();
        let Some(ext) = candidate_path.extension().and_then(|e| e.to_str()) else { continue };
        if !adapter.extensions.contains(&ext) {
            continue;
        }
        let Some(cached) = caches.parse.get(&candidate_path, adapter.language) else { continue };
        let (content, tree) = cached.as_ref();
        if let Some(want) = package_filter {
            if autoreview_archgraph::declared_package(content).as_deref() != Some(want) {
                continue;
            }
        }
        let hash = content_hash(content);
        if let Some(cached_summaries) = caches.persistent.get(rule_id, &candidate_path, &hash) {
            summaries.extend(cached_summaries);
            continue;
        }
        let mut file_summaries = HashMap::new();
        let source = content.as_bytes();
        for fn_node in (adapter.extract_functions)(tree) {
            if should_summarize.is_some_and(|gate| !gate(fn_node)) {
                continue;
            }
            if let Some(name) = (adapter.function_name)(fn_node, source) {
                let cfg = (adapter.lower_function)(source, fn_node);
                file_summaries.insert(name, compute_summary(&cfg));
            }
        }
        caches.persistent.put(rule_id, &candidate_path, hash, file_summaries.clone());
        summaries.extend(file_summaries);
    }
    summaries
}

/// Package-wide (same-directory) summaries for every function
/// `should_summarize` accepts (or every function, if `None`), scanning
/// every `.go` file in `file_path`'s directory (Go's directory=package
/// convention) that declares the *same* package as `file_path` itself
/// (excluding, e.g., a sibling `package foo_test` file — see
/// `scan_dir_for_summaries`'s own doc comment). Generic over
/// `compute_summary` (rule engine roadmap item 6) so both
/// `typed-nil-interface-return` and every Go taint rule's own
/// interprocedural resolution share this one implementation instead of
/// each hand-rolling its own directory scan.
fn go_package_summaries(
    repo_root: &Path,
    file_path: &str,
    should_summarize: Option<fn(Node) -> bool>,
    compute_summary: &dyn Fn(&Cfg<Stmt>) -> bool,
    rule_id: &str,
    caches: &SummaryCaches,
) -> HashMap<String, bool> {
    let full_path = repo_root.join(file_path);
    let Some(dir) = full_path.parent().map(Path::to_path_buf) else { return HashMap::new() };
    let own_package = caches.parse.get(&full_path, autoreview_langsupport::Language::Go).and_then(|cached| autoreview_archgraph::declared_package(&cached.0));
    scan_dir_for_summaries(&dir, &GO_ADAPTER, own_package.as_deref(), should_summarize, compute_summary, rule_id, caches)
}

/// Generalizes same-package resolution to the module's *other* internal
/// packages too, via `file_path`'s own import statements — a qualified
/// call (`pkg.Func(...)`) to a function declared in a different package
/// entirely was previously always an unresolved boundary (never flagged),
/// even when that package is part of the same module and its source is
/// right there to scan. Keyed by `"pkgname.Func"`, matching the qualified
/// form `lower::go::call_target_name` already produces for a selector
/// call, so lookups against these summaries and same-package ones share
/// one map with no special-casing at the call site. Generic over
/// `compute_summary` for the same reason as `go_package_summaries`.
///
/// Only imports resolving under this repo's own module path are followed
/// (an external or stdlib import has no source here to scan, and no
/// relevant function of ours could be behind it anyway). The package name
/// a qualified call uses is assumed to be the import path's last segment
/// — Go's overwhelmingly common convention, but not a language guarantee;
/// a package whose own `package` declaration disagrees with its directory
/// name (a rare, deliberately confusing pattern) won't resolve through
/// this heuristic.
fn go_imported_package_summaries(
    repo_root: &Path,
    file_path: &str,
    should_summarize: Option<fn(Node) -> bool>,
    compute_summary: &dyn Fn(&Cfg<Stmt>) -> bool,
    rule_id: &str,
    caches: &SummaryCaches,
) -> HashMap<String, bool> {
    let mut summaries = HashMap::new();
    let Some(module_path) = autoreview_archgraph::discover_go_module_path(repo_root) else { return summaries };
    let Some(cached) = caches.parse.get(&repo_root.join(file_path), autoreview_langsupport::Language::Go) else { return summaries };
    let content = &cached.0;
    for import in autoreview_archgraph::extract_go_imports(content) {
        let Some(suffix) = import.strip_prefix(&module_path) else { continue };
        let suffix = suffix.trim_start_matches('/');
        if suffix.is_empty() {
            continue;
        }
        let Some(pkg_name) = suffix.rsplit('/').next() else { continue };
        for (name, summary) in scan_dir_for_summaries(&repo_root.join(suffix), &GO_ADAPTER, None, should_summarize, compute_summary, rule_id, caches) {
            summaries.insert(format!("{pkg_name}.{name}"), summary);
        }
    }
    summaries
}

fn run_typed_nil_interface_return(file: &SummarySource, lowered: &[(Node, Cfg<Stmt>)]) -> Vec<AgentFinding> {
    // Pass 1: same-package summaries plus every other in-module package
    // this file imports — see `go_package_summaries`/
    // `go_imported_package_summaries` above.
    let returns_pointer: Option<fn(Node) -> bool> = Some(autoreview_dataflow::lower::go::function_returns_pointer);
    let rule_id = "typed-nil-interface-return";
    let summaries = file.go_summaries(returns_pointer, &go_typed_nil_interface_return::compute_summary, rule_id);

    // Pass 2: check every function declaring an `error` return against
    // those summaries.
    let mut findings = Vec::new();
    for (fn_node, cfg) in lowered {
        if !autoreview_dataflow::lower::go::function_returns_error(*fn_node, file.content.as_bytes()) {
            continue;
        }
        for hit in go_typed_nil_interface_return::check(cfg, &summaries) {
            findings.push(make_finding(
                "typed-nil-interface-return",
                "correctness",
                Severity::High,
                file.path,
                hit.source_line,
                format!("Returning typed pointer `{}` where an `error` is expected", hit.var),
                format!(
                    "This function declares an `error` return, but returns `{}` — a pointer variable that may be nil (either declared locally with no initializer, or assigned from a call to a function whose own return path can produce a nil pointer) — directly instead of an `error`-typed value. An `error` interface value holding a nil `*T` is itself non-nil (interfaces are a `(type, value)` pair internally), so a caller's `if err != nil` check passes even when `{}` is nil and nothing actually went wrong. Return a literal `nil` when there's no error, or explicitly convert: `if {} != nil {{ return {} }}; return nil`.",
                    hit.var, hit.var, hit.var, hit.var
                ),
            ));
        }
    }
    findings
}

/// Adds every function/method summary from one `.java`/`.kt`/`.kts` file
/// into `summaries` (keyed by bare function name — see
/// `autoreview_dataflow::lower::java::function_name`'s docs on the
/// accepted same-name-conflates-across-classes imprecision this shares
/// with Go's own summary keying), dispatching to the right
/// `LanguageAdapter` by extension. Generic over `compute_summary` (rule
/// engine roadmap item 6), used both by NPE-risk's own cross-package
/// resolution and, now, every Java/Kotlin taint rule's — used where a
/// concrete file list (from `find_java_kotlin_files_declaring_package`)
/// is already in hand rather than a directory to scan, so it can't go
/// through `scan_dir_for_summaries` directly.
fn add_java_kotlin_summaries_from_file(path: &Path, summaries: &mut HashMap<String, bool>, compute_summary: &dyn Fn(&Cfg<Stmt>) -> bool, rule_id: &str, caches: &SummaryCaches) {
    let adapter = match path.extension().and_then(|e| e.to_str()) {
        Some("java") => &JAVA_ADAPTER,
        Some("kt") | Some("kts") => &KOTLIN_ADAPTER,
        _ => return,
    };
    let Some(cached) = caches.parse.get(path, adapter.language) else { return };
    let (content, tree) = cached.as_ref();
    let hash = content_hash(content);
    if let Some(cached_summaries) = caches.persistent.get(rule_id, path, &hash) {
        summaries.extend(cached_summaries);
        return;
    }
    let mut file_summaries = HashMap::new();
    let source = content.as_bytes();
    for fn_node in (adapter.extract_functions)(tree) {
        if let Some(name) = (adapter.function_name)(fn_node, source) {
            let cfg = (adapter.lower_function)(source, fn_node);
            file_summaries.insert(name, compute_summary(&cfg));
        }
    }
    caches.persistent.put(rule_id, path, hash, file_summaries.clone());
    summaries.extend(file_summaries);
}

/// Same-package summaries for `file_path`, feeding pass 1 of both
/// `run_npe_risk` and (rule engine roadmap item 6) every Java/Kotlin
/// taint rule's own interprocedural resolution: every `.java`/`.kt`/
/// `.kts` file in the same directory whose own declared package matches
/// `own_package` (a mixed Java/Kotlin package — common in real Gradle/
/// Android projects — is scanned via both adapters and merged) — a fast
/// common-case scan (mirroring Go's own-directory scan) with a
/// correctness check on top, since directory == package isn't a language
/// guarantee here the way it is for Go (see `autoreview_archgraph`'s
/// module docs on why Java/Kotlin's package resolution reads the real
/// declaration instead of inferring from the path). Fully delegates to
/// the shared `scan_dir_for_summaries` (rule engine roadmap item 2)
/// rather than its own hand-rolled directory walk.
fn java_kotlin_package_summaries(
    repo_root: &Path,
    file_path: &str,
    own_package: &str,
    compute_summary: &dyn Fn(&Cfg<Stmt>) -> bool,
    rule_id: &str,
    caches: &SummaryCaches,
) -> HashMap<String, bool> {
    let Some(dir) = repo_root.join(file_path).parent().map(Path::to_path_buf) else { return HashMap::new() };
    let mut summaries = scan_dir_for_summaries(&dir, &JAVA_ADAPTER, Some(own_package), None, compute_summary, rule_id, caches);
    summaries.extend(scan_dir_for_summaries(&dir, &KOTLIN_ADAPTER, Some(own_package), None, compute_summary, rule_id, caches));
    summaries
}

/// Cross-package summaries: every package `own_content`'s file imports,
/// resolved to the real files declaring it anywhere in the repo —
/// `find_java_kotlin_files_declaring_package`'s own docs cover why this
/// is a whole-repo scan rather than a module-path-relative directory jump
/// the way Go's cross-package resolution is. Generic over
/// `compute_summary` for the same reason as `java_kotlin_package_summaries`.
/// Summarizes functions in packages imported by a Java or Kotlin file.
fn java_kotlin_imported_package_summaries(
    repo_root: &Path,
    own_content: &str,
    compute_summary: &dyn Fn(&Cfg<Stmt>) -> bool,
    rule_id: &str,
    caches: &SummaryCaches,
) -> HashMap<String, bool> {
    let mut summaries = HashMap::new();
    for import in autoreview_archgraph::extract_java_kotlin_imports(own_content) {
        let pkg = autoreview_archgraph::import_package(&import);
        let packages = caches.java_kotlin_packages.get_or_init(|| autoreview_archgraph::index_java_kotlin_packages(repo_root));
        for path in packages.get(&pkg).into_iter().flatten() {
            add_java_kotlin_summaries_from_file(path, &mut summaries, compute_summary, rule_id, caches);
        }
    }
    summaries
}

/// Interprocedural NPE-risk check for one Java/Kotlin file — see
/// `autoreview_dataflow::rules::java_kotlin_npe_risk`'s module docs for
/// the two-pass design this wires up. Pass 1's summaries come from
/// `java_kotlin_package_summaries` (same directory, filtered by declared
/// package) merged with `java_kotlin_imported_package_summaries` (every
/// other package this file imports); a file with no `package` declaration
/// at all (Java's default/unnamed package) falls back to summarizing just
/// its own functions, so same-file resolution still works even without
/// cross-file scope.
fn run_npe_risk(file: &SummarySource, lowered: &[(Node, Cfg<Stmt>)], null_safe_methods: &[&str]) -> Vec<AgentFinding> {
    let compute = &autoreview_dataflow::rules::java_kotlin_npe_risk::compute_summary;
    let rule_id = "npe-risk-from-helper-return";
    let summaries = file.java_kotlin_summaries(compute, rule_id);
    let mut findings = Vec::new();
    for (_, cfg) in lowered {
        for hit in autoreview_dataflow::rules::java_kotlin_npe_risk::check(cfg, &summaries, null_safe_methods) {
            findings.push(make_finding(
                "npe-risk-from-helper-return",
                "correctness",
                Severity::High,
                file.path,
                hit.source_line,
                format!("`{}` may be null after `{}`", hit.var, hit.call_name),
                format!(
                    "`{}` was assigned from `{}`, and that call has a path that returns null. Using it here without a null check risks a `NullPointerException`. Add `if ({} != null) {{ ... }}` before this use, or change the helper's contract so it never returns null (throw instead, or return an `Optional`).",
                    hit.var, hit.call_name, hit.var
                ),
            ));
        }
    }
    findings
}

/// Reads and parses one file, for the checks below to lower — a shared
/// early-return-on-any-failure helper since all three language branches in
/// `run_dataflow_check` do exactly this before lowering. Routed through
/// `ParseCache` so a changed file that's also a sibling/imported
/// dependency of another changed file in the same diff is only read and
/// parsed once across the whole `run_dataflow_check` call, not once per
/// role it plays.
fn read_and_parse(repo_root: &Path, path: &str, language: autoreview_langsupport::Language, cache: &ParseCache) -> Option<ParsedFile> {
    cache.get(&repo_root.join(path), language)
}

/// Runs all dataflow-powered checks against one changed file's current
/// content. Go gets the full rule set (Phase 3/4/5: append-shared-
/// backing-array, typed-nil-interface-return, loopvar, plus taint rules).
/// Java/Kotlin also get the interprocedural NPE-risk check
/// (`java_kotlin_npe_risk`, the Java/Kotlin analog of Go's typed-nil-
/// return rule — see that module's docs). All supported CFG languages
/// run both taint and call-sequence rules. Parses and lowers each
/// file's functions exactly once per language, shared across that
/// language's rule families rather than each re-parsing/re-lowering
/// independently — and, via `ParseCache`, shared across every
/// interprocedural rule's own same-package/cross-package resolution too
/// (rule engine roadmap item 1, see `RULE_ENGINE_RESEARCH.md`).
pub fn run_dataflow_check(repo_root: &Path, changed_files: &[String], registered_packs: &[ResolvedRulePack]) -> Vec<AgentFinding> {
    run_dataflow_check_for_snapshot(repo_root, repo_root, changed_files, registered_packs)
}

pub fn run_dataflow_check_for_snapshot(repo: &Path, repo_root: &Path, changed_files: &[String], registered_packs: &[ResolvedRulePack]) -> Vec<AgentFinding> {
    run_dataflow_check_with_coverage(repo, repo_root, changed_files, registered_packs).findings
}

pub struct DataflowReviewResult {
    pub findings: Vec<AgentFinding>,
    pub coverage: Vec<autoreview_schema::AnalysisCoverageIssue>,
}

pub fn run_dataflow_check_with_coverage(repo: &Path, repo_root: &Path, changed_files: &[String], registered_packs: &[ResolvedRulePack]) -> DataflowReviewResult {
    dispatch::run(repo, repo_root, changed_files, registered_packs)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use super::cache::summary_cache_path;
    use super::*;

    #[test]
    fn reports_unreadable_source_and_invalid_imported_syntax() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("invalid.go"), [0xff, 0xfe]).unwrap();
        let unreadable = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["invalid.go".into()], &[]);
        assert!(unreadable.coverage.iter().any(|item| item.reason == "source_unavailable" && item.path.as_deref() == Some("invalid.go")));
        std::fs::write(dir.path().join("main.ts"), "import { helper } from './dep';\nexport function run() { helper(); }\n").unwrap();
        std::fs::write(dir.path().join("dep.ts"), "export function helper( {\n").unwrap();
        let imported = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.ts".into()], &[]);
        assert!(imported.coverage.iter().any(|item| item.reason == "dependency_syntax_errors" && item.path.as_deref() == Some("dep.ts")), "{:?}", imported.coverage);
    }

    #[test]
    fn reports_unmodeled_cleanup_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let pack = tempfile::tempdir().unwrap();
        std::fs::write(pack.path().join("pending.yml"), "id: pending-entry\nkind: call-sequence\nlanguage: JavaScript\ncategory: correctness\nseverity: warning\nmessage: Pending entry\nafter: [{call: register}]\nunless: [{call: remove}]\ncheckBeforeReturn: true\nidentity: {receiver: true, keyArgument: 0}\n").unwrap();
        for argument in ["flag ? choose(key) : key", "unknown(key)"] {
            std::fs::write(dir.path().join("main.js"), format!("function choose(key) {{ return key; }}\nfunction pending(root, key, flag) {{ root.register(key); root.remove({argument}); }}\n")).unwrap();
            let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.js".into()], &[ResolvedRulePack { id: "test".into(), local_path: pack.path().into(), trust: autoreview_schema::RulePackTrust::Full }]);
            assert!(result.coverage.iter().any(|item| item.reason == "unmodeled_path" && item.rule.as_deref() == Some("pending-entry")), "{argument}: {:?}", result.coverage);
        }
    }

    #[test]
    fn ambiguous_reexports_are_not_resolved_and_explicit_exports_take_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let pack = tempfile::tempdir().unwrap();
        std::fs::write(pack.path().join("pending.yml"), "id: pending-entry\nkind: call-sequence\nlanguage: JavaScript\ncategory: correctness\nseverity: warning\nmessage: Pending entry\nafter: [{call: register}]\nunless: [{call: remove}]\ncheckBeforeReturn: true\nidentity: {receiver: true, keyArgument: 0}\n").unwrap();
        std::fs::write(dir.path().join("main.js"), "import { choose } from './index';\nfunction pending(root, key) { root.register(key); root.remove(choose(key)); }\n").unwrap();
        std::fs::write(dir.path().join("a.js"), "export function choose(key) { return 'wrong'; }\n").unwrap();
        std::fs::write(dir.path().join("b.js"), "export function choose(key) { return key; }\n").unwrap();
        std::fs::write(dir.path().join("c.js"), "export const choose = 1;\n").unwrap();
        std::fs::write(dir.path().join("d.js"), "export function choose(first, second) { return second; }\n").unwrap();
        let packs = [ResolvedRulePack { id: "test".into(), local_path: pack.path().into(), trust: autoreview_schema::RulePackTrust::Full }];
        for (barrel, incomplete, finding) in [
            ("export * from './a'; export * from './b';", true, false),
            ("export * from './a'; export * from './c';", true, false),
            ("export * from './a'; export * from './d';", true, false),
            ("export * from './a'; export { choose } from './b';", false, false),
            ("export * from './b'; export { choose } from './a';", false, true),
            ("export * from './a'; export const choose = 1;", true, false),
            ("export const choose = key => 'wrong';", false, true),
            ("const choose = key => 'wrong'; export const other = 1;", true, false),
            ("export let choose = key => 'wrong';", true, false),
            ("export const choose = async key => 'wrong';", true, false),
            ("export const choose = async function(key) { return 'wrong'; };", true, false),
            ("export * from './a'; export const { choose } = source;", true, false),
            ("export * from './a'; export * as choose from './b';", true, false),
            ("export * from './a'; export * from './missing';", true, false),
            ("export * from './a'; export * from 'external-package';", true, false),
        ] {
            std::fs::write(dir.path().join("index.js"), barrel).unwrap();
            let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.js".into()], &packs);
            assert_eq!(result.coverage.iter().any(|item| item.reason == "unmodeled_path" && item.rule.as_deref() == Some("pending-entry")), incomplete, "{barrel}: {:?}", result.coverage);
            assert_eq!(result.findings.iter().any(|item| item.source.rule_id.as_deref() == Some("pending-entry")), finding, "{barrel}: {:?}", result.findings);
        }
        for level in 0..15 {
            let body = if level == 14 {
                "export { choose } from './a';".to_owned()
            } else {
                format!("export * from './deep_a{}'; export * from './deep_b{}';", level + 1, level + 1)
            };
            for side in ["a", "b"] {
                std::fs::write(dir.path().join(format!("deep_{side}{level}.js")), &body).unwrap();
            }
        }
        std::fs::write(dir.path().join("index.js"), "export * from './deep_a0'; export * from './deep_b0';").unwrap();
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.js".into()], &packs);
        assert!(result.coverage.iter().any(|item| item.reason == "reexport_resolution_limit" && item.rule.as_deref() == Some("pending-entry")), "{:?}", result.coverage);
        assert!(!result.findings.iter().any(|item| item.source.rule_id.as_deref() == Some("pending-entry")));
    }

    #[test]
    fn reports_unresolved_local_web_imports_without_flagging_packages_or_assets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("present.ts"), "export function helper() {}\n").unwrap();
        std::fs::write(dir.path().join("component.jsx"), "export function component() {}\n").unwrap();
        std::fs::write(dir.path().join("types.ts"), "import { helper } from './missing-type-dependency';\nexport type Shape = typeof helper;\n").unwrap();
        std::fs::write(dir.path().join("main.ts"), r#"import { helper } from './present.js';
import { missing } from './missing';
import { absent } from '../absent';
import { external } from 'some-package';
import './side-effects';
import styles from './styles.css';
import type { Shape } from './types';
import { type Other } from './other-types';
import { type Data, work } from './mixed';
import { component } from './component';
export function run() { helper(); missing(); absent(); external(); }
"#).unwrap();
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.ts".into()], &[]);
        let unresolved: Vec<_> = result.coverage.iter().filter(|item| item.reason == "unresolved_local_import").collect();
        assert_eq!(unresolved.len(), 3, "{:?}", result.coverage);
        assert!(unresolved.iter().any(|item| item.path.as_deref() == Some("main.ts") && item.line == Some(2) && item.detail.contains("./missing")));
        assert!(unresolved.iter().any(|item| item.path.as_deref() == Some("main.ts") && item.line == Some(3) && item.detail.contains("../absent")));
        assert!(unresolved.iter().any(|item| item.path.as_deref() == Some("main.ts") && item.line == Some(9) && item.detail.contains("./mixed")));
    }

    #[test]
    fn reports_unresolved_and_unsupported_runtime_reexports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.ts"), "import { run } from './index';\nfunction main() { run(); }\n").unwrap();
        std::fs::write(dir.path().join("index.ts"), "export * from './missing';\nexport type { Shape } from './types';\nexport * as nested from './present';\n").unwrap();
        std::fs::write(dir.path().join("present.ts"), "export function run() {}\n").unwrap();
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.ts".into()], &[]);
        assert!(result.coverage.iter().any(|item| item.reason == "unresolved_reexport" && item.path.as_deref() == Some("index.ts") && item.line == Some(1)), "{:?}", result.coverage);
        assert!(result.coverage.iter().any(|item| item.reason == "unsupported_reexport" && item.path.as_deref() == Some("index.ts") && item.line == Some(3)), "{:?}", result.coverage);
        assert!(!result.coverage.iter().any(|item| item.path.as_deref() == Some("index.ts") && item.line == Some(2)), "{:?}", result.coverage);
    }

    #[test]
    fn reports_when_imported_source_limit_leaves_dependencies_unanalyzed() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..67 {
            let source = if index < 66 {
                format!("import {{ helper }} from './f{}';\nexport function run() {{ helper(); }}\n", index + 1)
            } else {
                "export function helper() {}\n".to_owned()
            };
            std::fs::write(dir.path().join(format!("f{index}.ts")), source).unwrap();
        }
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["f0.ts".into()], &[]);
        assert!(result.coverage.iter().any(|item| item.reason == "dependency_limit" && item.scope == "dependency"), "{:?}", result.coverage);
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["f2.ts".into()], &[]);
        assert!(!result.coverage.iter().any(|item| item.reason == "dependency_limit"));
    }

    #[test]
    fn go_helper_filter_keeps_unparseable_dependencies_and_avoids_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package app\nfunc run() { helper() }\n").unwrap();
        std::fs::write(dir.path().join("broken.go"), "package app\nfunc broken( {\n").unwrap();
        for index in 0..80 {
            std::fs::write(dir.path().join(format!("unused_{index:03}.go")), format!("package app\nfunc unused_{index:03}() {{}}\n")).unwrap();
        }
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.go".into()], &[]);
        assert!(result.coverage.iter().any(|item| item.reason == "dependency_syntax_errors" && item.path.as_deref() == Some("broken.go")), "{:?}", result.coverage);
        assert!(!result.coverage.iter().any(|item| item.reason == "dependency_limit"), "{:?}", result.coverage);
        let calls = (0..80).map(|index| format!("unused_{index:03}();")).collect::<String>();
        std::fs::write(dir.path().join("main.go"), format!("package app\nfunc run() {{ {calls} }}\n")).unwrap();
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.go".into()], &[]);
        assert!(result.coverage.iter().any(|item| item.reason == "dependency_limit"), "{:?}", result.coverage);
    }

    #[test]
    fn reports_go_dependencies_when_the_package_cannot_be_determined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.test/project\n\ngo 1.22\n").unwrap();
        std::fs::write(dir.path().join("main.go"), "package app\nimport \"example.test/project/lib\"\nfunc run() { lib.Work() }\n").unwrap();
        std::fs::write(dir.path().join("second.go"), "package app\nimport \"example.test/project/lib\"\nfunc next() { lib.Work() }\n").unwrap();
        std::fs::write(dir.path().join("unreadable.go"), [0xff, 0xfe]).unwrap();
        std::fs::write(dir.path().join("lib/unreadable.go"), [0xff, 0xfe]).unwrap();
        std::fs::write(dir.path().join("missing_package.go"), "func broken( {\n").unwrap();
        std::fs::write(dir.path().join("lib/missing_package.go"), "func broken( {\n").unwrap();
        std::fs::write(dir.path().join("lib/empty.go"), "// package declaration is missing\n").unwrap();
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.go".into(), "second.go".into()], &[]);
        for (path, reason) in [
            ("unreadable.go", "dependency_unavailable"),
            ("lib/unreadable.go", "dependency_unavailable"),
            ("missing_package.go", "dependency_syntax_errors"),
            ("lib/missing_package.go", "dependency_syntax_errors"),
            ("lib/empty.go", "dependency_package_unavailable"),
        ] {
            assert_eq!(result.coverage.iter().filter(|item| item.path.as_deref() == Some(path) && item.reason == reason).count(), 1, "expected one {reason} for {path}: {:?}", result.coverage);
        }
    }

    #[test]
    fn reports_functions_skipped_when_finally_lowering_budget_is_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = String::from("function work(x: number) { try {\n");
        for index in 0..140 {
            source.push_str(&format!("if (x === {index}) return;\n"));
        }
        source.push_str("} finally { cleanup(); } }\n");
        std::fs::write(dir.path().join("main.ts"), source).unwrap();
        let result = run_dataflow_check_with_coverage(dir.path(), dir.path(), &["main.ts".into()], &[]);
        assert!(result.coverage.iter().any(|item| item.reason == "control_flow_limit"
            && item.path.as_deref() == Some("main.ts") && item.line == Some(1)), "{:?}", result.coverage);
    }

    #[test]
    fn parse_cache_returns_the_same_parse_on_repeated_lookups_of_the_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.go");
        std::fs::write(&path, "package main\n\nfunc f() {}\n").unwrap();

        let cache = ParseCache::new();
        let first = cache.get(&path, autoreview_langsupport::Language::Go).unwrap();
        let second = cache.get(&path, autoreview_langsupport::Language::Go).unwrap();
        assert!(Rc::ptr_eq(&first, &second), "a second lookup of the same path must reuse the cached parse, not re-read/re-parse the file");
    }

    #[test]
    fn parse_cache_caches_a_miss_too_so_a_missing_file_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ParseCache::new();
        assert!(cache.get(&dir.path().join("nonexistent.go"), autoreview_langsupport::Language::Go).is_none());
        // A second lookup of the same missing path must also cleanly return
        // None (not panic on a stale cache entry, and not attempt a second
        // real filesystem read — verified indirectly: this only matters if
        // some other assertion about the cache's internal HashMap.len()
        // would be relevant, which it isn't here; the contract under test
        // is just "repeated misses stay misses, no panic").
        assert!(cache.get(&dir.path().join("nonexistent.go"), autoreview_langsupport::Language::Go).is_none());
    }

    #[test]
    fn persistent_summary_cache_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let next_snapshot = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.go");
        let same_file = next_snapshot.path().join("f.go");
        let cache_path = summary_cache_path(dir.path());
        let _ = std::fs::remove_file(&cache_path);

        {
            let cache = PersistentSummaryCache::load(dir.path(), dir.path());
            assert!(cache.get("rule-a", &file, "hash1").is_none());
            cache.put("rule-a", &file, "hash1".to_string(), HashMap::from([("f".to_string(), true)]));
            cache.save();
        }

        let reloaded = PersistentSummaryCache::load(dir.path(), next_snapshot.path());
        assert_eq!(reloaded.get("rule-a", &same_file, "hash1"), Some(HashMap::from([("f".to_string(), true)])), "a matching content hash must return the persisted summary in a new snapshot");
        assert!(reloaded.get("rule-a", &same_file, "hash2").is_none(), "a different content hash must miss");
        assert!(reloaded.get("rule-b", &same_file, "hash1").is_none(), "a different rule id must miss");

        let _ = std::fs::remove_file(&cache_path);
    }

    #[test]
    fn persistent_summary_cache_survives_a_missing_or_corrupt_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = summary_cache_path(dir.path());
        let _ = std::fs::create_dir_all(cache_path.parent().unwrap());
        std::fs::write(&cache_path, "not valid json").unwrap();

        let cache = PersistentSummaryCache::load(dir.path(), dir.path());
        assert!(cache.get("rule-a", Path::new("/tmp/f.go"), "hash1").is_none(), "a corrupt cache file must degrade to an empty cache, not panic or propagate an error");

        let _ = std::fs::remove_file(&cache_path);
    }

    #[test]
    fn run_dataflow_check_persists_cross_package_summaries_for_reuse_on_a_later_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = summary_cache_path(dir.path());
        let _ = std::fs::remove_file(&cache_path);

        std::fs::write(dir.path().join("helper.go"), "package main\n\nfunc mightFail() *Thing {\n\tvar t *Thing\n\treturn t\n}\n\ntype Thing struct{}\n").unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f() error {\n\tt := mightFail()\n\treturn t\n}\n").unwrap();

        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")), "got: {findings:#?}");

        // A second, entirely separate invocation (fresh ParseCache, as a
        // real second CLI run would have) must reuse the persisted
        // summary rather than recomputing it from scratch — verified
        // directly against the persisted cache file rather than via
        // instrumentation, since content-hash correctness means the
        // *findings* are identical either way.
        let helper_content = std::fs::read_to_string(dir.path().join("helper.go")).unwrap();
        let persisted = PersistentSummaryCache::load(dir.path(), dir.path());
        let cached = persisted.get("typed-nil-interface-return", &dir.path().join("helper.go"), &content_hash(&helper_content));
        assert_eq!(cached.as_ref().and_then(|s| s.get("mightFail")), Some(&true), "got: {cached:#?}");

        // A second invocation must still find the same finding, sourced
        // from the persisted cache this time.
        let findings2 = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings2.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")), "got: {findings2:#?}");

        let _ = std::fs::remove_file(&cache_path);
    }

    #[test]
    fn flags_append_that_may_overwrite_a_reused_slices_backing_array() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f(full []int) []int {\n\tsub := full[2:4]\n\tsub = append(sub, 9)\n\tprintln(full[0])\n\treturn sub\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("append-shared-backing-array"));
    }

    #[test]
    fn does_not_flag_after_sub_is_reassigned() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc f(full []int, other []int) []int {\n\tsub := full[2:4]\n\tsub = other\n\tsub = append(sub, 9)\n\tprintln(full[0])\n\treturn sub\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.is_empty(), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_same_function_typed_nil_return() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc do() error {\n\tvar e *myError\n\tif somethingBad {\n\t\te = &myError{}\n\t}\n\treturn e\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")), "got: {findings:#?}");
    }

    #[test]
    fn flags_an_interprocedural_typed_nil_return_across_two_functions_in_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc helper() *myError {\n\tvar e *myError\n\treturn e\n}\n\nfunc Do() error {\n\te := helper()\n\treturn e\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")),
            "got: {findings:#?} — same-function-only heuristic couldn't have caught this"
        );
    }

    #[test]
    fn flags_an_interprocedural_typed_nil_return_across_two_files_in_the_same_package() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("helper.go"), "package main\n\nfunc helper() *myError {\n\tvar e *myError\n\treturn e\n}\n").unwrap();
        std::fs::write(dir.path().join("caller.go"), "package main\n\nfunc Do() error {\n\te := helper()\n\treturn e\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["caller.go".to_string()], &[]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")),
            "got: {findings:#?} — same-file-only resolution couldn't have caught this, `helper` is declared in a sibling file"
        );
    }

    #[test]
    fn does_not_resolve_a_helper_declared_in_a_sibling_external_test_package() {
        // Regression test for the package_filter added when
        // scan_dir_for_pointer_summaries was generalized into
        // scan_dir_for_summaries: a directory can legally hold both
        // `package main` and an external test package `package main_test`
        // side by side (Go's own convention) — a same-named helper
        // declared in the *test* package must not resolve for the real
        // package's own same-package summary lookup.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("helper_test.go"), "package main_test\n\nfunc helper() *myError {\n\tvar e *myError\n\treturn e\n}\n").unwrap();
        std::fs::write(dir.path().join("caller.go"), "package main\n\nfunc Do() error {\n\te := helper()\n\treturn e\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["caller.go".to_string()], &[]);
        assert!(
            !findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")),
            "got: {findings:#?} — `helper` is declared in a different (test) package in the same directory, must stay an unresolved boundary"
        );
    }

    #[test]
    fn flags_an_interprocedural_typed_nil_return_across_two_different_packages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\n\ngo 1.22\n").unwrap();
        std::fs::create_dir_all(dir.path().join("helper")).unwrap();
        std::fs::write(dir.path().join("helper/helper.go"), "package helper\n\nfunc Helper() *myError {\n\tvar e *myError\n\treturn e\n}\n").unwrap();
        std::fs::write(
            dir.path().join("caller.go"),
            "package main\n\nimport \"example.com/x/helper\"\n\nfunc Do() error {\n\te := helper.Helper()\n\treturn e\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["caller.go".to_string()], &[]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")),
            "got: {findings:#?} — same-package-only resolution couldn't have caught this, `helper.Helper` is declared in a different package entirely"
        );
    }

    #[test]
    fn does_not_flag_the_guarded_idiom_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc do() error {\n\tvar e *myError\n\tif somethingBad {\n\t\te = &myError{}\n\t}\n\tif e != nil {\n\t\treturn e\n\t}\n\treturn nil\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_same_file_java_npe_risk_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "class Foo {\n    void f() {\n        Object e = null;\n        e.toString();\n    }\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.java".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_guarded_java_dereference_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "class Foo {\n    void f() {\n        Object e = null;\n        if (e != null) {\n            e.toString();\n        }\n    }\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.java".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")), "got: {findings:#?}");
    }

    #[test]
    fn flags_an_interprocedural_java_npe_risk_within_the_same_class() {
        // Java's bare (receiver-less) call syntax only ever resolves within
        // the same class (implicit `this.`) — a cross-class call always
        // needs a receiver (`helper.risky()`), which this rule can't
        // resolve without type information (see the module's own doc
        // comment on why summaries are keyed by bare name). So the
        // realistic Java interprocedural case this rule actually catches is
        // two methods on the same class — the cross-file/cross-package
        // proof below uses Kotlin instead, where a bare call to a
        // top-level function genuinely can cross files without a receiver.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Foo.java"),
            "class Foo {\n    Object risky() {\n        Object e = null;\n        return e;\n    }\n    void f() {\n        Object e = risky();\n        e.toString();\n    }\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.java".to_string()], &[]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")),
            "got: {findings:#?} — a same-function-only heuristic couldn't have caught this, risky() is a different method"
        );
    }

    #[test]
    fn npe_package_summaries_scans_both_java_and_kotlin_files_in_the_same_directory() {
        // A mixed Java/Kotlin package (common in real Gradle/Android
        // projects) must be resolved through both LanguageAdapters and
        // merged into one summary map — this is the case
        // java_kotlin_package_summaries's generalization onto
        // scan_dir_for_summaries (rule engine roadmap item 2) exists to
        // cover directly, since a single end-to-end run_dataflow_check
        // test can't easily exercise real cross-language bare-call
        // resolution (Java/Kotlin interop doesn't produce a bare-callable
        // shape either language's side of this rule resolves — see the
        // same-class-only Java test above).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("JavaHelper.java"), "package com.example;\n\nclass JavaHelper {\n    Object risky() {\n        Object e = null;\n        return e;\n    }\n}\n").unwrap();
        std::fs::write(dir.path().join("KotlinHelper.kt"), "package com.example\n\nfun riskyKt(): String? {\n    val e: String? = null\n    return e\n}\n").unwrap();
        let cache = ParseCache::new();
        let persistent = PersistentSummaryCache::load(dir.path(), dir.path());
        let caches = SummaryCaches { parse: &cache, persistent: &persistent, java_kotlin_packages: Default::default() };
        let summaries = java_kotlin_package_summaries(dir.path(), "Caller.java", "com.example", &autoreview_dataflow::rules::java_kotlin_npe_risk::compute_summary, "npe-risk-from-helper-return", &caches);
        assert_eq!(summaries.get("risky"), Some(&true), "got: {summaries:#?}");
        assert_eq!(summaries.get("riskyKt"), Some(&true), "got: {summaries:#?}");
    }

    #[test]
    fn flags_a_same_file_kotlin_npe_risk_end_to_end() {
        // Deliberately not .toString() — Kotlin's stdlib defines
        // Any?.toString() as a genuinely null-safe extension (never
        // throws), so it's excluded from this rule for Kotlin specifically
        // (see run_npe_risk's own Kotlin call site). Note this specific
        // source wouldn't actually compile in real Kotlin (a bare
        // .hashCode() on an explicitly-`?`-typed variable needs `?.`/`!!`)
        // — this test exercises the rule's mechanism, not a realistic
        // Kotlin snippet; see java_kotlin_npe_risk's module doc comment
        // for the honest caveat about this rule's narrower practical
        // surface for pure Kotlin-to-Kotlin calls.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.kt"), "class Foo {\n    fun f() {\n        val e: String? = null\n        e.hashCode()\n    }\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.kt".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_kotlin_tostring_call_on_a_nullable_receiver() {
        // Regression test for the null_safe_methods exclusion: unlike
        // every other method, Any?.toString() never NPEs in Kotlin, so
        // this must not fire even with no guard in between.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.kt"), "class Foo {\n    fun f() {\n        val e: String? = null\n        e.toString()\n    }\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.kt".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_guarded_kotlin_dereference_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Foo.kt"),
            "class Foo {\n    fun f() {\n        val e: String? = null\n        if (e != null) {\n            e.toString()\n        }\n    }\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.kt".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")), "got: {findings:#?}");
    }

    #[test]
    fn flags_an_interprocedural_kotlin_npe_risk_across_two_files_in_the_same_package() {
        // Unlike Java, a Kotlin top-level function has no enclosing class,
        // so a bare call to it genuinely can resolve across files with no
        // receiver — this is where the same-package/cross-package summary
        // machinery actually earns its keep for Kotlin.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Helper.kt"), "package com.example\n\nfun risky(): String? {\n    val e: String? = null\n    return e\n}\n").unwrap();
        std::fs::write(dir.path().join("Caller.kt"), "package com.example\n\nfun f() {\n    val e = risky()\n    e.hashCode()\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Caller.kt".to_string()], &[]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")),
            "got: {findings:#?} — same-file-only resolution couldn't have caught this, risky() is declared in a sibling file"
        );
    }

    #[test]
    fn flags_an_interprocedural_kotlin_npe_risk_across_two_different_packages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("com/example/helper")).unwrap();
        std::fs::write(dir.path().join("com/example/helper/Helper.kt"), "package com.example.helper\n\nfun risky(): String? {\n    val e: String? = null\n    return e\n}\n").unwrap();
        std::fs::create_dir_all(dir.path().join("com/example/app")).unwrap();
        std::fs::write(
            dir.path().join("com/example/app/Caller.kt"),
            "package com.example.app\n\nimport com.example.helper.risky\n\nfun f() {\n    val e = risky()\n    e.hashCode()\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["com/example/app/Caller.kt".to_string()], &[]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("npe-risk-from-helper-return")),
            "got: {findings:#?} — same-package-only resolution couldn't have caught this, risky() is declared in a different package entirely"
        );
    }

    #[test]
    fn flags_a_loopvar_capture_when_go_mod_targets_pre_1_22() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\n\ngo 1.20\n").unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("loopvar-capture-pre-1.22")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_loopvar_address_capture_when_go_mod_targets_pre_1_22() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\n\ngo 1.20\n").unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc f(items []string) []*string {\n\tvar out []*string\n\tfor _, item := range items {\n\t\tout = append(out, &item)\n\t}\n\treturn out\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("loopvar-address-pre-1.22")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_loopvar_capture_when_go_mod_targets_1_22_or_later() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\n\ngo 1.23\n").unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("loopvar-capture-pre-1.22")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_loopvar_capture_without_a_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("loopvar-capture-pre-1.22")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_form_value_reaching_exec_command_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc handle(r *http.Request) {\n\tuserInput := r.FormValue(\"cmd\")\n\texec.Command(\"sh\", \"-c\", userInput)\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_source_hidden_inside_a_niladic_helper_reaching_exec_command_end_to_end() {
        // Rule engine roadmap item 6: getUserCommand() takes no
        // arguments at all, so the pre-existing intraprocedural default
        // (taint auto-propagates through an unrecognized call only from
        // an already-tainted argument) has nothing to look at — only the
        // new interprocedural source-summary resolution can see that
        // getUserCommand()'s own body wraps a real taint source.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc getUserCommand(r *http.Request) string {\n\tcmd := r.FormValue(\"cmd\")\n\treturn cmd\n}\n\nfunc handle() {\n\tuserInput := getUserCommand(nil)\n\texec.Command(\"sh\", \"-c\", userInput)\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")),
            "got: {findings:#?} — intraprocedural-only taint tracking couldn't have caught this, the source is hidden inside getUserCommand()'s own body"
        );
    }

    #[test]
    fn does_not_flag_a_literal_only_exec_command_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f() {\n\texec.Command(\"ls\", \"-la\")\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_form_value_reaching_an_exec_cmd_struct_literals_path_field_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc handle(r *http.Request) {\n\tuserInput := r.FormValue(\"cmd\")\n\tc := &exec.Cmd{Path: userInput}\n\tc.Run()\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_an_exec_cmd_struct_literal_with_only_a_literal_path_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f() {\n\tc := &exec.Cmd{Path: \"/bin/ls\"}\n\tc.Run()\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_form_value_reaching_sql_query_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc handle(r *http.Request, db *sql.DB) {\n\tid := r.FormValue(\"id\")\n\trows, err := db.Query(id)\n\t_ = rows\n\t_ = err\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-sql-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_concatenated_query_reaching_sql_exec_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc handle(r *http.Request, db *sql.DB) {\n\tid := r.FormValue(\"id\")\n\tq := \"DELETE FROM users WHERE id=\" + id\n\tres, err := db.Exec(q)\n\t_ = res\n\t_ = err\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-sql-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_form_value_reaching_os_open_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc handle(r *http.Request) {\n\tname := r.FormValue(\"file\")\n\tf, err := os.Open(name)\n\t_ = f\n\t_ = err\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-path-traversal-taint")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_hardcoded_path_or_query_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc f(db *sql.DB) {\n\trows, err := db.Query(\"SELECT * FROM users\")\n\t_ = rows\n\t_ = err\n\tdata, err2 := os.ReadFile(\"/etc/config.json\")\n\t_ = data\n\t_ = err2\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-sql-injection-taint") || f.source.rule_id.as_deref() == Some("go-path-traversal-taint")), "got: {findings:#?}");
    }

    #[test]
    fn skips_non_go_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "class Foo {}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.java".to_string()], &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn a_pack_sourced_taint_finding_carries_rule_pack_id_in_meta() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc f() {\n\tv := os.Getenv(\"SECRET\")\n\tfmt.Println(v)\n}\n").unwrap();

        let pack_dir = dir.path().join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join("rulepack.yaml"), "id: acme-taint\nversion: \"1.0.0\"\n").unwrap();
        std::fs::write(
            pack_dir.join("env-taint.yml"),
            "id: acme-env-taint\nkind: taint\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nsources:\n  - call: Getenv\nsinks:\n  - call: Println\nsanitizers: []\n",
        )
        .unwrap();
        let packs = vec![crate::rule_packs::ResolvedRulePack { id: "acme-taint".to_string(), local_path: pack_dir, trust: autoreview_schema::RulePackTrust::Full }];

        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &packs);
        let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some("acme-env-taint")).unwrap_or_else(|| panic!("got: {findings:#?}"));
        let meta = finding.meta.as_ref().expect("expected meta to carry rulePackId");
        assert_eq!(meta.get("rulePackId").and_then(|v| v.as_str()), Some("acme-taint"));
    }

    /// The counterpart to the pack test above: a *builtin* rule must not
    /// claim a `rulePackId`. This previously asserted `meta.is_none()`,
    /// which only held while `meta` had nothing else to carry — builtin
    /// taint rules now declare CWE metadata, so the absence of a pack id
    /// is asserted directly rather than via an empty map.
    #[test]
    fn a_builtin_taint_finding_carries_its_cwe_but_no_pack_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nimport (\n\t\"net/http\"\n\t\"os/exec\"\n)\n\nfunc f(r *http.Request) {\n\tcmd := r.FormValue(\"cmd\")\n\texec.Command(cmd)\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")).unwrap_or_else(|| panic!("got: {findings:#?}"));
        let meta = finding.meta.as_ref().expect("go-command-injection-taint declares cwe metadata, so its finding carries meta");
        assert!(!meta.contains_key("rulePackId"), "a builtin rule must not claim a pack id, got: {meta:?}");
        assert_eq!(meta.get("cwe"), Some(&serde_json::json!(["CWE-78"])), "the rule's declared CWE must reach the finding, got: {meta:?}");
    }

    #[test]
    fn flags_a_request_parameter_reaching_a_java_sql_sink_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Main.java"),
            "class Main {\n    void handle(HttpServletRequest req, Statement stmt) {\n        String q = req.getParameter(\"q\");\n        stmt.executeQuery(q);\n    }\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["Main.java".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("java-sql-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_java_sql_sink_reached_with_a_literal_query() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Main.java"), "class Main {\n    void handle(Statement stmt) {\n        stmt.executeQuery(\"SELECT 1\");\n    }\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Main.java".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("java-sql-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_request_parameter_reaching_a_kotlin_sql_sink_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Main.kt"),
            "class Main {\n    fun handle(req: HttpServletRequest, stmt: Statement) {\n        val q = req.getParameter(\"q\")\n        stmt.executeQuery(q)\n    }\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["Main.kt".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("kotlin-sql-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_kotlin_sql_sink_reached_with_a_literal_query() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Main.kt"), "class Main {\n    fun handle(stmt: Statement) {\n        stmt.executeQuery(\"SELECT 1\")\n    }\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Main.kt".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("kotlin-sql-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_request_parameter_reaching_a_javascript_command_injection_sink_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.js"), "function handle(req, cp) {\n  const cmd = req.param(\"cmd\");\n  cp.exec(cmd);\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.js".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("javascript-command-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_javascript_sink_reached_with_a_literal_argument() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.js"), "function handle(cp) {\n  cp.exec(\"ls -la\");\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.js".to_string()], &[]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("javascript-command-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_request_parameter_reaching_a_typescript_sql_sink_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.ts"), "function handle(req: any, db: any) {\n  const q: string = req.param(\"q\");\n  db.query(q);\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.ts".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typescript-sql-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_request_parameter_reaching_a_tsx_command_injection_sink_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tsx"), "function handle(req: any, cp: any) {\n  const cmd: string = req.param(\"cmd\");\n  cp.execSync(cmd);\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.tsx".to_string()], &[]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typescript-command-injection-taint")), "got: {findings:#?}");
    }
}
