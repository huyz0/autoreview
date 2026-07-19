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
//! `typed-nil-interface-return`'s interprocedural call resolution is
//! same-file only (see `autoreview_dataflow::rules::
//! go_typed_nil_interface_return`'s module docs for the two-pass design)
//! — a call to a function resolvable in the SymbolIndex but declared in
//! a different file of the same package is currently treated as an
//! unknown boundary (not flagged), same as a call to a genuinely
//! external package. Extending resolution to same-package-different-file
//! via `autoreview_symindex::SymbolIndex` is a tracked follow-up, not
//! done in this pass.

use std::collections::HashMap;
use std::path::Path;

use autoreview_dataflow::rules::{go_append_shared_backing_array, go_command_injection_taint, go_loopvar, go_path_traversal_taint, go_sql_injection_taint, go_typed_nil_interface_return};
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

fn make_finding(rule_id: &str, path: &str, line: u32, title: String, message: String) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-dataflow".to_string(), rule_id: Some(rule_id.to_string()), aspect: None, backend: None },
        category: "style".to_string(),
        severity: Severity::Low,
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

fn run_append_shared_backing_array(path: &str, content: &str) -> Vec<AgentFinding> {
    let Some(mut parser) = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go) else { return Vec::new() };
    let Some(tree) = parser.parse(content, None) else { return Vec::new() };
    let mut findings = Vec::new();
    for fn_node in go_functions(&tree) {
        let cfg = autoreview_dataflow::lower::go::lower_function(content.as_bytes(), fn_node);
        for hit in go_append_shared_backing_array::check(&cfg) {
            findings.push(make_finding(
                "append-shared-backing-array",
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

fn run_typed_nil_interface_return(path: &str, content: &str) -> Vec<AgentFinding> {
    let Some(mut parser) = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go) else { return Vec::new() };
    let Some(tree) = parser.parse(content, None) else { return Vec::new() };
    let source = content.as_bytes();
    let functions = go_functions(&tree);

    // Pass 1: same-file summaries for every pointer-returning function.
    let mut summaries: HashMap<String, bool> = HashMap::new();
    for fn_node in &functions {
        if autoreview_dataflow::lower::go::function_returns_pointer(*fn_node) {
            if let Some(name) = autoreview_dataflow::lower::go::function_name(*fn_node, source) {
                let cfg = autoreview_dataflow::lower::go::lower_function(source, *fn_node);
                summaries.insert(name, go_typed_nil_interface_return::compute_summary(&cfg));
            }
        }
    }

    // Pass 2: check every function declaring an `error` return against
    // those summaries.
    let mut findings = Vec::new();
    for fn_node in &functions {
        if !autoreview_dataflow::lower::go::function_returns_error(*fn_node, source) {
            continue;
        }
        let cfg = autoreview_dataflow::lower::go::lower_function(source, *fn_node);
        for hit in go_typed_nil_interface_return::check(&cfg, &summaries) {
            findings.push(make_finding(
                "typed-nil-interface-return",
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

fn run_loopvar_checks(path: &str, content: &str) -> Vec<AgentFinding> {
    let Some(mut parser) = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go) else { return Vec::new() };
    let Some(tree) = parser.parse(content, None) else { return Vec::new() };
    let mut findings = Vec::new();
    for fn_node in go_functions(&tree) {
        let cfg = autoreview_dataflow::lower::go::lower_function(content.as_bytes(), fn_node);

        for hit in go_loopvar::check_capture(&cfg) {
            let kind = if hit.kind == autoreview_dataflow::cfg::ClosureKind::Goroutine { "goroutine" } else { "deferred closure" };
            findings.push(make_finding(
                "loopvar-capture-pre-1.22",
                path,
                hit.source_line,
                format!("Loop variable `{}` captured by a {kind} on a pre-1.22 Go module", hit.var),
                format!(
                    "This {kind} references the enclosing loop's `{}` without shadowing it first, and `go.mod` targets a Go version before 1.22. Before 1.22, `for`/`range` loop variables have per-loop (not per-iteration) scope — every {kind} launched by this loop ends up seeing the same, final value of `{}` instead of its own iteration's value. Shadow it first (`{} := {}`) right inside the closure, or pass it as a parameter.",
                    hit.var, hit.var, hit.var, hit.var
                ),
            ));
        }

        for hit in go_loopvar::check_address(&cfg) {
            findings.push(make_finding(
                "loopvar-address-pre-1.22",
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

/// Shared shape for every taint-powered Go rule: parse, lower every
/// function, run `spec` over each, map hits to findings via `message`.
fn run_taint_spec(rule_id: &str, spec: &autoreview_dataflow::taint::TaintSpec, path: &str, content: &str, message: impl Fn(&autoreview_dataflow::taint::TaintHit) -> (String, String)) -> Vec<AgentFinding> {
    let Some(mut parser) = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go) else { return Vec::new() };
    let Some(tree) = parser.parse(content, None) else { return Vec::new() };
    let mut findings = Vec::new();
    for fn_node in go_functions(&tree) {
        let cfg = autoreview_dataflow::lower::go::lower_function(content.as_bytes(), fn_node);
        for hit in autoreview_dataflow::taint::check(spec, &cfg) {
            let (title, msg) = message(&hit);
            findings.push(make_finding(rule_id, path, hit.source_line, title, msg));
        }
    }
    findings
}

fn run_command_injection_taint(path: &str, content: &str) -> Vec<AgentFinding> {
    run_taint_spec("go-command-injection-taint", &go_command_injection_taint::spec(), path, content, |hit| {
        (
            format!("`{}` reaches `{}` with an unsanitized value from an HTTP form field", hit.tainted_arg, hit.sink_call),
            format!(
                "`{}` is derived from `r.FormValue(...)`/`r.PostFormValue(...)` (attacker-controlled request data) and reaches `{}` without going through a sanitizer first. If this value ends up in the executed command's argument list, an attacker can inject arbitrary shell arguments or a different binary entirely. Validate/allowlist the value before it reaches `{}`, or avoid passing request-derived data into it at all.",
                hit.tainted_arg, hit.sink_call, hit.sink_call
            ),
        )
    })
}

fn run_sql_injection_taint(path: &str, content: &str) -> Vec<AgentFinding> {
    run_taint_spec("go-sql-injection-taint", &go_sql_injection_taint::spec(), path, content, |hit| {
        (
            format!("`{}` reaches `{}` with an unsanitized value from an HTTP form field", hit.tainted_arg, hit.sink_call),
            format!(
                "`{}` is derived from `r.FormValue(...)`/`r.PostFormValue(...)` and reaches `{}` — either passed directly as (part of) the query, or concatenated into a query string first. Use a parameterized query (`?` placeholders) instead of building the query text from request-derived data, even indirectly through a variable.",
                hit.tainted_arg, hit.sink_call
            ),
        )
    })
}

fn run_path_traversal_taint(path: &str, content: &str) -> Vec<AgentFinding> {
    run_taint_spec("go-path-traversal-taint", &go_path_traversal_taint::spec(), path, content, |hit| {
        (
            format!("`{}` reaches `{}` with an unsanitized value from an HTTP form field", hit.tainted_arg, hit.sink_call),
            format!(
                "`{}` is derived from `r.FormValue(...)`/`r.PostFormValue(...)` and reaches `{}` — either directly or concatenated into a path first — without validation. An attacker can supply `../` segments to escape the intended directory. Validate the resulting path stays within an expected base directory before this call, not just that it was built from a \"cleaned\" string.",
                hit.tainted_arg, hit.sink_call
            ),
        )
    })
}

/// Runs all dataflow-powered checks against one changed file's current
/// content. Go-only for now (Phase 3/4/5); Java/Kotlin land once their
/// lowering passes do (Phase 6/7).
pub fn run_dataflow_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    let go_pre_1_22 = crate::analyzers::practices::go_module_targets_pre_1_22(repo_root);
    changed_files
        .iter()
        .filter(|path| path.ends_with(".go"))
        .filter_map(|path| {
            let full_path = repo_root.join(path);
            let content = std::fs::read_to_string(&full_path).ok()?;
            let mut findings = run_append_shared_backing_array(path, &content);
            findings.extend(run_typed_nil_interface_return(path, &content));
            findings.extend(run_command_injection_taint(path, &content));
            findings.extend(run_sql_injection_taint(path, &content));
            findings.extend(run_path_traversal_taint(path, &content));
            if go_pre_1_22 {
                findings.extend(run_loopvar_checks(path, &content));
            }
            Some(findings)
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_append_that_may_overwrite_a_reused_slices_backing_array() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f(full []int) []int {\n\tsub := full[2:4]\n\tsub = append(sub, 9)\n\tprintln(full[0])\n\treturn sub\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
        assert!(
            findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")),
            "got: {findings:#?} — same-function-only heuristic couldn't have caught this"
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("typed-nil-interface-return")), "got: {findings:#?}");
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-command-injection-taint")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_literal_only_exec_command_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f() {\n\texec.Command(\"ls\", \"-la\")\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
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
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-sql-injection-taint") || f.source.rule_id.as_deref() == Some("go-path-traversal-taint")), "got: {findings:#?}");
    }

    #[test]
    fn skips_non_go_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "class Foo {}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.java".to_string()]);
        assert!(findings.is_empty());
    }
}
