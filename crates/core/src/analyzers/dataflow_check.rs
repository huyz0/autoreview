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
//! `run_loaded_taint_rules` — adding a new taint rule for a language that
//! already has a lowering pass doesn't touch this file at all.
//!
//! Java (`lower::java`), Kotlin (`lower::kotlin`), and JavaScript/
//! TypeScript/TSX (`lower::javascript`, one lowering module shared across
//! all three — see its own doc comment) only get taint rules run against
//! them for now — see `run_dataflow_check`'s own doc comment.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tree_sitter::{Node, Tree};

use autoreview_dataflow::cfg::{Cfg, Stmt};
use autoreview_dataflow::rules::{go_append_shared_backing_array, go_loopvar, go_typed_nil_interface_return};
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

use super::taint_rules;
use crate::rule_packs::ResolvedRulePack;

/// A read+parse cache shared across one `run_dataflow_check` invocation —
/// Rule engine roadmap item 1 (see `RULE_ENGINE_RESEARCH.md`): every
/// interprocedural rule's same-package/cross-package resolution
/// (`package_summaries`, `imported_package_summaries`,
/// `npe_package_summaries`, `npe_imported_package_summaries`) previously
/// independently re-read and re-parsed the same sibling/imported files
/// from scratch, once per changed file that needed them — if N changed
/// files in one diff import the same package, that package's files got
/// parsed N times. This cache makes each file's read+parse pay its cost
/// at most once per `run_dataflow_check` call, regardless of how many
/// changed files or rules end up needing it. `Rc` rather than a plain
/// owned return because `Tree`'s clone is cheap (an internal refcount
/// bump, backed by tree-sitter's own C library) but re-parsing is not —
/// sharing one parse via `Rc` avoids paying for a second clone of the
/// tree on every cache hit.
type ParsedFile = Rc<(String, Tree)>;

struct ParseCache {
    entries: RefCell<HashMap<PathBuf, Option<ParsedFile>>>,
}

impl ParseCache {
    fn new() -> Self {
        ParseCache { entries: RefCell::new(HashMap::new()) }
    }

    /// Returns the cached (content, tree) for `path`, parsing and caching
    /// it on first request. `None` on a read or parse failure — cached as
    /// `None` too, so a missing/unparseable file isn't retried on every
    /// subsequent lookup within this run.
    fn get(&self, path: &Path, language: autoreview_langsupport::Language) -> Option<ParsedFile> {
        if let Some(cached) = self.entries.borrow().get(path) {
            return cached.clone();
        }
        let parsed = (|| {
            let content = std::fs::read_to_string(path).ok()?;
            let mut parser = autoreview_langsupport::parser_for(language)?;
            let tree = parser.parse(&content, None)?;
            Some(Rc::new((content, tree)))
        })();
        self.entries.borrow_mut().insert(path.to_path_buf(), parsed.clone());
        parsed
    }
}

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

/// Builds the `meta` map carrying `rulePackId` when `rule`'s taint rule
/// definition came from a registered pack — `None` for builtin rules,
/// matching `ast_grep.rs`'s own pattern-rule provenance tagging.
fn taint_pack_meta(rule: &taint_rules::TaintRuleDef) -> Option<HashMap<String, serde_json::Value>> {
    let pack_id = rule.pack_id.as_ref()?;
    let mut meta = HashMap::new();
    meta.insert("rulePackId".to_string(), serde_json::Value::String(pack_id.clone()));
    Some(meta)
}

/// Every top-level `func`/method declaration in a parsed Go file.
fn go_functions(tree: &tree_sitter::Tree) -> Vec<tree_sitter::Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = tree.root_node().walk();
    for node in tree.root_node().named_children(&mut cursor) {
        if node.kind() == "function_declaration" || node.kind() == "method_declaration" {
            out.push(node);
        }
    }
    out
}

/// Parses and lowers every function in the file exactly once, shared by
/// all four rule families below — previously each family independently
/// re-parsed the file and re-lowered every function's CFG, up to 4x
/// redundant work per file for logic that all operates on the same
/// underlying functions.
fn lower_all_functions<'a>(source: &[u8], tree: &'a Tree) -> Vec<(Node<'a>, Cfg<Stmt>)> {
    go_functions(tree).into_iter().map(|fn_node| (fn_node, autoreview_dataflow::lower::go::lower_function(source, fn_node))).collect()
}

/// Every `method_declaration`/`constructor_declaration` anywhere in a
/// parsed Java file, at any nesting depth — unlike Go's top-level-only
/// `function_declaration`s, Java methods always sit inside a
/// `class_declaration`'s `class_body` (possibly several classes deep for
/// nested/inner classes), so this walks the whole tree rather than just
/// `root_node()`'s immediate children.
fn java_functions(tree: &Tree) -> Vec<Node<'_>> {
    fn walk<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
        if node.kind() == "method_declaration" || node.kind() == "constructor_declaration" {
            out.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree.root_node(), &mut out);
    out
}

/// Every `function_declaration` anywhere in a parsed Kotlin file — same
/// nested-class rationale as `java_functions`.
fn kotlin_functions(tree: &Tree) -> Vec<Node<'_>> {
    fn walk<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
        if node.kind() == "function_declaration" {
            out.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree.root_node(), &mut out);
    out
}

/// Every `function_declaration` anywhere in a parsed JavaScript/TypeScript/
/// TSX file — same nested-scope rationale as `java_functions`, since a
/// function can sit inside a module, namespace, or another function's body.
fn javascript_functions(tree: &Tree) -> Vec<Node<'_>> {
    fn walk<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
        if node.kind() == "function_declaration" {
            out.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree.root_node(), &mut out);
    out
}

fn lower_all_java_functions<'a>(source: &[u8], tree: &'a Tree) -> Vec<(Node<'a>, Cfg<Stmt>)> {
    java_functions(tree).into_iter().map(|fn_node| (fn_node, autoreview_dataflow::lower::java::lower_function(source, fn_node))).collect()
}

fn lower_all_kotlin_functions<'a>(source: &[u8], tree: &'a Tree) -> Vec<(Node<'a>, Cfg<Stmt>)> {
    kotlin_functions(tree).into_iter().map(|fn_node| (fn_node, autoreview_dataflow::lower::kotlin::lower_function(source, fn_node))).collect()
}

fn lower_all_javascript_functions<'a>(source: &[u8], tree: &'a Tree) -> Vec<(Node<'a>, Cfg<Stmt>)> {
    javascript_functions(tree).into_iter().map(|fn_node| (fn_node, autoreview_dataflow::lower::javascript::lower_function(source, fn_node))).collect()
}

fn run_append_shared_backing_array(path: &str, lowered: &[(Node, Cfg<Stmt>)]) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for (_, cfg) in lowered {
        for hit in go_append_shared_backing_array::check(cfg) {
            findings.push(make_finding(
                "append-shared-backing-array",
                "correctness",
                Severity::Medium,
                path,
                hit.source_line,
                format!("`{} = append({}, ...)` may overwrite `{}`'s backing array", hit.sub, hit.sub, hit.full),
                format!(
                    "`{}` was created by re-slicing `{}` (`{} := {}[...]`), and `{}` is still used later in this function. `append({}, ...)` may reuse `{}`'s shared backing array when there's spare capacity, silently overwriting memory `{}` still references. Use `{} := append([]T{{}}, {}[a:b]...)` (or `copy`) to force a fresh backing array if `{}` needs to stay untouched, or restructure so `{}` doesn't outlive its use.",
                    hit.sub, hit.full, hit.sub, hit.full, hit.full, hit.sub, hit.sub, hit.full, hit.sub, hit.full, hit.full, hit.sub
                ),
            ));
        }
    }
    findings
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
    extract_functions: kotlin_functions,
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
fn scan_dir_for_summaries(dir: &Path, adapter: &LanguageAdapter, package_filter: Option<&str>, should_summarize: Option<fn(Node) -> bool>, compute_summary: fn(&Cfg<Stmt>) -> bool, cache: &ParseCache) -> HashMap<String, bool> {
    let mut summaries = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return summaries };
    for entry in entries.filter_map(Result::ok) {
        let candidate_path = entry.path();
        let Some(ext) = candidate_path.extension().and_then(|e| e.to_str()) else { continue };
        if !adapter.extensions.contains(&ext) {
            continue;
        }
        let Some(cached) = cache.get(&candidate_path, adapter.language) else { continue };
        let (content, tree) = cached.as_ref();
        if let Some(want) = package_filter {
            if autoreview_archgraph::declared_package(content).as_deref() != Some(want) {
                continue;
            }
        }
        let source = content.as_bytes();
        for fn_node in (adapter.extract_functions)(tree) {
            if should_summarize.is_some_and(|gate| !gate(fn_node)) {
                continue;
            }
            if let Some(name) = (adapter.function_name)(fn_node, source) {
                let cfg = (adapter.lower_function)(source, fn_node);
                summaries.insert(name, compute_summary(&cfg));
            }
        }
    }
    summaries
}

fn scan_dir_for_pointer_summaries(dir: &Path, package_filter: Option<&str>, cache: &ParseCache) -> HashMap<String, bool> {
    scan_dir_for_summaries(dir, &GO_ADAPTER, package_filter, Some(autoreview_dataflow::lower::go::function_returns_pointer), go_typed_nil_interface_return::compute_summary, cache)
}

/// Package-wide (same-directory) summaries for every pointer-returning
/// function, feeding pass 1 of `run_typed_nil_interface_return` below —
/// scans every `.go` file in `file_path`'s directory (Go's
/// directory=package convention) that declares the *same* package as
/// `file_path` itself (excluding, e.g., a sibling `package foo_test`
/// file — see `scan_dir_for_summaries`'s own doc comment), including
/// `file_path` itself, rather than just the one file being checked.
fn package_summaries(repo_root: &Path, file_path: &str, cache: &ParseCache) -> HashMap<String, bool> {
    let full_path = repo_root.join(file_path);
    let Some(dir) = full_path.parent().map(Path::to_path_buf) else { return HashMap::new() };
    let own_package = cache.get(&full_path, autoreview_langsupport::Language::Go).and_then(|cached| autoreview_archgraph::declared_package(&cached.0));
    scan_dir_for_pointer_summaries(&dir, own_package.as_deref(), cache)
}

/// Generalizes same-package resolution to the module's *other* internal
/// packages too, via `file_path`'s own import statements — a qualified
/// call (`pkg.Func(...)`) to a function declared in a different package
/// entirely was previously always an unresolved boundary (never flagged),
/// even when that package is part of the same module and its source is
/// right there to scan. Keyed by `"pkgname.Func"`, matching the qualified
/// form `lower::go::call_target_name` already produces for a selector
/// call, so lookups against these summaries and same-package ones share
/// one map with no special-casing at the call site.
///
/// Only imports resolving under this repo's own module path are followed
/// (an external or stdlib import has no source here to scan, and no
/// pointer-returning function of ours could be behind it anyway). The
/// package name a qualified call uses is assumed to be the import path's
/// last segment — Go's overwhelmingly common convention, but not a
/// language guarantee; a package whose own `package` declaration disagrees
/// with its directory name (a rare, deliberately confusing pattern) won't
/// resolve through this heuristic.
fn imported_package_summaries(repo_root: &Path, file_path: &str, cache: &ParseCache) -> HashMap<String, bool> {
    let mut summaries = HashMap::new();
    let Some(module_path) = autoreview_archgraph::discover_go_module_path(repo_root) else { return summaries };
    let Some(cached) = cache.get(&repo_root.join(file_path), autoreview_langsupport::Language::Go) else { return summaries };
    let content = &cached.0;
    for import in autoreview_archgraph::extract_go_imports(content) {
        let Some(suffix) = import.strip_prefix(&module_path) else { continue };
        let suffix = suffix.trim_start_matches('/');
        if suffix.is_empty() {
            continue;
        }
        let Some(pkg_name) = suffix.rsplit('/').next() else { continue };
        for (name, summary) in scan_dir_for_pointer_summaries(&repo_root.join(suffix), None, cache) {
            summaries.insert(format!("{pkg_name}.{name}"), summary);
        }
    }
    summaries
}

fn run_typed_nil_interface_return(repo_root: &Path, path: &str, source: &[u8], lowered: &[(Node, Cfg<Stmt>)], cache: &ParseCache) -> Vec<AgentFinding> {
    // Pass 1: same-package summaries plus every other in-module package
    // this file imports — see `package_summaries`/`imported_package_summaries`
    // above.
    let mut summaries = package_summaries(repo_root, path, cache);
    summaries.extend(imported_package_summaries(repo_root, path, cache));

    // Pass 2: check every function declaring an `error` return against
    // those summaries.
    let mut findings = Vec::new();
    for (fn_node, cfg) in lowered {
        if !autoreview_dataflow::lower::go::function_returns_error(*fn_node, source) {
            continue;
        }
        for hit in go_typed_nil_interface_return::check(cfg, &summaries) {
            findings.push(make_finding(
                "typed-nil-interface-return",
                "correctness",
                Severity::High,
                path,
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
/// with Go's own summary keying). Best-effort: a file that fails to read
/// or parse is silently skipped, same "current file's findings still
/// matter" rationale as Go's `scan_dir_for_pointer_summaries`.
/// Adds every function/method summary from one `.java`/`.kt`/`.kts` file
/// into `summaries`, dispatching to the right `LanguageAdapter` by
/// extension — used by `npe_imported_package_summaries` below, which
/// already has a concrete file list (from
/// `find_java_kotlin_files_declaring_package`) rather than a directory to
/// scan, so it can't go through `scan_dir_for_summaries` directly.
fn add_npe_summaries_from_file(path: &Path, summaries: &mut HashMap<String, bool>, cache: &ParseCache) {
    let adapter = match path.extension().and_then(|e| e.to_str()) {
        Some("java") => &JAVA_ADAPTER,
        Some("kt") | Some("kts") => &KOTLIN_ADAPTER,
        _ => return,
    };
    let Some(cached) = cache.get(path, adapter.language) else { return };
    let (content, tree) = cached.as_ref();
    let source = content.as_bytes();
    for fn_node in (adapter.extract_functions)(tree) {
        if let Some(name) = (adapter.function_name)(fn_node, source) {
            let cfg = (adapter.lower_function)(source, fn_node);
            summaries.insert(name, autoreview_dataflow::rules::java_kotlin_npe_risk::compute_summary(&cfg));
        }
    }
}

/// Same-package summaries for `file_path`'s NPE-risk pass 1: every
/// `.java`/`.kt`/`.kts` file in the same directory whose own declared
/// package matches `own_package` (a mixed Java/Kotlin package — common in
/// real Gradle/Android projects — is scanned via both adapters and
/// merged) — a fast common-case scan (mirroring Go's own-directory scan)
/// with a correctness check on top, since directory == package isn't a
/// language guarantee here the way it is for Go (see
/// `autoreview_archgraph`'s module docs on why Java/Kotlin's package
/// resolution reads the real declaration instead of inferring from the
/// path). Fully delegates to the shared `scan_dir_for_summaries` (rule
/// engine roadmap item 2) rather than its own hand-rolled directory walk.
fn npe_package_summaries(repo_root: &Path, file_path: &str, own_package: &str, cache: &ParseCache) -> HashMap<String, bool> {
    let Some(dir) = repo_root.join(file_path).parent().map(Path::to_path_buf) else { return HashMap::new() };
    let mut summaries = scan_dir_for_summaries(&dir, &JAVA_ADAPTER, Some(own_package), None, autoreview_dataflow::rules::java_kotlin_npe_risk::compute_summary, cache);
    summaries.extend(scan_dir_for_summaries(&dir, &KOTLIN_ADAPTER, Some(own_package), None, autoreview_dataflow::rules::java_kotlin_npe_risk::compute_summary, cache));
    summaries
}

/// Cross-package summaries: every package `own_content`'s file imports,
/// resolved to the real files declaring it anywhere in the repo —
/// `find_java_kotlin_files_declaring_package`'s own docs cover why this
/// is a whole-repo scan rather than a module-path-relative directory jump
/// the way Go's cross-package resolution is.
fn npe_imported_package_summaries(repo_root: &Path, own_content: &str, cache: &ParseCache) -> HashMap<String, bool> {
    let mut summaries = HashMap::new();
    for import in autoreview_archgraph::extract_java_kotlin_imports(own_content) {
        let pkg = autoreview_archgraph::import_package(&import);
        for path in autoreview_archgraph::find_java_kotlin_files_declaring_package(repo_root, &pkg) {
            add_npe_summaries_from_file(&path, &mut summaries, cache);
        }
    }
    summaries
}

/// Interprocedural NPE-risk check for one Java/Kotlin file — see
/// `autoreview_dataflow::rules::java_kotlin_npe_risk`'s module docs for
/// the two-pass design this wires up. Pass 1's summaries come from
/// `npe_package_summaries` (same directory, filtered by declared package)
/// merged with `npe_imported_package_summaries` (every other package this
/// file imports); a file with no `package` declaration at all (Java's
/// default/unnamed package) falls back to summarizing just its own
/// functions, so same-file resolution still works even without cross-file
/// scope.
fn run_npe_risk(repo_root: &Path, path: &str, own_content: &str, lowered: &[(Node, Cfg<Stmt>)], null_safe_methods: &[&str], cache: &ParseCache) -> Vec<AgentFinding> {
    let mut summaries = match autoreview_archgraph::declared_package(own_content) {
        Some(package) => npe_package_summaries(repo_root, path, &package, cache),
        None => {
            let mut own_only = HashMap::new();
            add_npe_summaries_from_file(&repo_root.join(path), &mut own_only, cache);
            own_only
        }
    };
    summaries.extend(npe_imported_package_summaries(repo_root, own_content, cache));

    let mut findings = Vec::new();
    for (_, cfg) in lowered {
        for hit in autoreview_dataflow::rules::java_kotlin_npe_risk::check(cfg, &summaries, null_safe_methods) {
            findings.push(make_finding(
                "npe-risk-from-helper-return",
                "correctness",
                Severity::High,
                path,
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

fn run_loopvar_checks(path: &str, lowered: &[(Node, Cfg<Stmt>)]) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for (_, cfg) in lowered {
        for hit in go_loopvar::check_capture(cfg) {
            let kind = if hit.kind == autoreview_dataflow::cfg::ClosureKind::Goroutine { "goroutine" } else { "deferred closure" };
            findings.push(make_finding(
                "loopvar-capture-pre-1.22",
                "correctness",
                Severity::Medium,
                path,
                hit.source_line,
                format!("Loop variable `{}` captured by a {kind} on a pre-1.22 Go module", hit.var),
                format!(
                    "This {kind} references the enclosing loop's `{}` without shadowing it first, and `go.mod` targets a Go version before 1.22. Before 1.22, `for`/`range` loop variables have per-loop (not per-iteration) scope — every {kind} launched by this loop ends up seeing the same, final value of `{}` instead of its own iteration's value. Shadow it first (`{} := {}`) right inside the closure, or pass it as a parameter.",
                    hit.var, hit.var, hit.var, hit.var
                ),
            ));
        }

        for hit in go_loopvar::check_address(cfg) {
            findings.push(make_finding(
                "loopvar-address-pre-1.22",
                "correctness",
                Severity::Medium,
                path,
                hit.source_line,
                format!("Address of loop variable `{}` taken on a pre-1.22 Go module", hit.var),
                format!(
                    "This takes `&{}` inside the loop that declares `{}`, and `go.mod` targets a Go version before 1.22. Before 1.22, `for`/`range` loop variables have per-loop (not per-iteration) scope — every `&{}` taken across iterations points at the same shared variable, so a slice/map built from these pointers ends up holding N copies of the loop's final value instead of each iteration's own value. Shadow the variable first (`{} := {}`) before taking its address, or upgrade the module's Go version.",
                    hit.var, hit.var, hit.var, hit.var, hit.var
                ),
            ));
        }
    }
    findings
}

/// Substitutes `{tainted_arg}`/`{sink_call}` in a rule's YAML `message`
/// template with the actual hit's values — the declarative-rule
/// equivalent of what the three old hand-written Rust closures each did
/// inline.
fn render_taint_message(template: &str, hit: &autoreview_dataflow::taint::TaintHit) -> String {
    template.replace("{tainted_arg}", &hit.tainted_arg).replace("{sink_call}", &hit.sink_call)
}

/// Deliberately doesn't name the source (e.g. "an HTTP form field") the
/// way an earlier, Go-only version of this string did — now that taint
/// rules cover multiple languages and source families (HTTP form values,
/// request parameters, ...), a single hardcoded source description would
/// misdescribe whichever rules don't match it. The rule's own `message`
/// (YAML `message:` field) is where a rule-specific, accurate source
/// description belongs.
fn taint_title(hit: &autoreview_dataflow::taint::TaintHit) -> String {
    format!("`{}` reaches `{}` with an unsanitized value from an untrusted source", hit.tainted_arg, hit.sink_call)
}

/// Runs every `kind: taint` rule declared in `rules-builtin/` or a
/// registered pack (loaded via `taint_rules::load_taint_rules`) whose
/// `language` matches, against one file's already-lowered functions.
/// Adding a new taint rule means adding a new YAML file — this function
/// doesn't change.
fn run_loaded_taint_rules(path: &str, language: &str, lowered: &[(Node, Cfg<Stmt>)], registered_packs: &[ResolvedRulePack]) -> Vec<AgentFinding> {
    let rules: Vec<_> = taint_rules::load_taint_rules(registered_packs).into_iter().filter(|r| r.language == language).collect();
    if rules.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for (_, cfg) in lowered {
        for rule in &rules {
            for hit in autoreview_dataflow::taint::check(&rule.spec, cfg) {
                let mut finding = make_finding(&rule.id, &rule.category, rule.severity, path, hit.source_line, taint_title(&hit), render_taint_message(&rule.message, &hit));
                finding.meta = taint_pack_meta(rule);
                findings.push(finding);
            }
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
/// return rule — see that module's docs). JavaScript/TypeScript/TSX only
/// get taint rules so far — the generic taint engine already works
/// against any lowered `Cfg`, so a `kind: taint` YAML rule for any of
/// them needs no dataflow-crate changes to run. Parses and lowers each
/// file's functions exactly once per language, shared across that
/// language's rule families rather than each re-parsing/re-lowering
/// independently — and, via `ParseCache`, shared across every
/// interprocedural rule's own same-package/cross-package resolution too
/// (rule engine roadmap item 1, see `RULE_ENGINE_RESEARCH.md`).
pub fn run_dataflow_check(repo_root: &Path, changed_files: &[String], registered_packs: &[ResolvedRulePack]) -> Vec<AgentFinding> {
    let go_pre_1_22 = crate::analyzers::practices::go_module_targets_pre_1_22(repo_root);
    let cache = ParseCache::new();
    changed_files
        .iter()
        .filter_map(|path| {
            if path.ends_with(".go") {
                let cached = read_and_parse(repo_root, path, autoreview_langsupport::Language::Go, &cache)?;
                let (content, tree) = cached.as_ref();
                let source = content.as_bytes();
                let lowered = lower_all_functions(source, tree);

                let mut findings = run_append_shared_backing_array(path, &lowered);
                findings.extend(run_typed_nil_interface_return(repo_root, path, source, &lowered, &cache));
                findings.extend(run_loaded_taint_rules(path, "Go", &lowered, registered_packs));
                if go_pre_1_22 {
                    findings.extend(run_loopvar_checks(path, &lowered));
                }
                Some(findings)
            } else if path.ends_with(".java") {
                let cached = read_and_parse(repo_root, path, autoreview_langsupport::Language::Java, &cache)?;
                let (content, tree) = cached.as_ref();
                let lowered = lower_all_java_functions(content.as_bytes(), tree);
                let mut findings = run_npe_risk(repo_root, path, content, &lowered, &[], &cache);
                findings.extend(run_loaded_taint_rules(path, "Java", &lowered, registered_packs));
                Some(findings)
            } else if path.ends_with(".kt") || path.ends_with(".kts") {
                let cached = read_and_parse(repo_root, path, autoreview_langsupport::Language::Kotlin, &cache)?;
                let (content, tree) = cached.as_ref();
                let lowered = lower_all_kotlin_functions(content.as_bytes(), tree);
                // Kotlin's stdlib defines Any?.toString() as a null-safe
                // extension (see java_kotlin_npe_risk's own docs) — the
                // only call this rule's dereference-walk must not treat as
                // risky on a nullable receiver in Kotlin specifically.
                let mut findings = run_npe_risk(repo_root, path, content, &lowered, &["toString"], &cache);
                findings.extend(run_loaded_taint_rules(path, "Kotlin", &lowered, registered_packs));
                Some(findings)
            } else if path.ends_with(".tsx") {
                let cached = read_and_parse(repo_root, path, autoreview_langsupport::Language::Tsx, &cache)?;
                let (content, tree) = cached.as_ref();
                let lowered = lower_all_javascript_functions(content.as_bytes(), tree);
                // Reuses the "TypeScript" taint rules rather than a separate
                // "Tsx" bucket — TSX's grammar only adds JSX syntax on top
                // of TypeScript's, so the same taint rules apply verbatim; a
                // duplicate set of Tsx-only YAML files would just be the
                // same rules twice.
                Some(run_loaded_taint_rules(path, "TypeScript", &lowered, registered_packs))
            } else if path.ends_with(".ts") || path.ends_with(".mts") || path.ends_with(".cts") {
                let cached = read_and_parse(repo_root, path, autoreview_langsupport::Language::TypeScript, &cache)?;
                let (content, tree) = cached.as_ref();
                let lowered = lower_all_javascript_functions(content.as_bytes(), tree);
                Some(run_loaded_taint_rules(path, "TypeScript", &lowered, registered_packs))
            } else if path.ends_with(".js") || path.ends_with(".jsx") || path.ends_with(".mjs") || path.ends_with(".cjs") {
                let cached = read_and_parse(repo_root, path, autoreview_langsupport::Language::JavaScript, &cache)?;
                let (content, tree) = cached.as_ref();
                let lowered = lower_all_javascript_functions(content.as_bytes(), tree);
                Some(run_loaded_taint_rules(path, "JavaScript", &lowered, registered_packs))
            } else {
                None
            }
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // npe_package_summaries's generalization onto scan_dir_for_summaries
        // (rule engine roadmap item 2) exists to cover directly, since a
        // single end-to-end run_dataflow_check test can't easily exercise
        // real cross-language bare-call resolution (Java/Kotlin
        // interop doesn't produce a bare-callable shape either language's
        // side of this rule resolves — see the same-class-only Java test
        // above).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("JavaHelper.java"), "package com.example;\n\nclass JavaHelper {\n    Object risky() {\n        Object e = null;\n        return e;\n    }\n}\n").unwrap();
        std::fs::write(dir.path().join("KotlinHelper.kt"), "package com.example\n\nfun riskyKt(): String? {\n    val e: String? = null\n    return e\n}\n").unwrap();
        let cache = ParseCache::new();
        let summaries = npe_package_summaries(dir.path(), "Caller.java", "com.example", &cache);
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

    #[test]
    fn a_builtin_taint_finding_has_no_meta() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nimport (\n\t\"net/http\"\n\t\"os/exec\"\n)\n\nfunc f(r *http.Request) {\n\tcmd := r.FormValue(\"cmd\")\n\texec.Command(cmd)\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()], &[]);
        let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")).unwrap_or_else(|| panic!("got: {findings:#?}"));
        assert!(finding.meta.is_none());
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
