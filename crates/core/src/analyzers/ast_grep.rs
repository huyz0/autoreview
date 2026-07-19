use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use include_dir::{include_dir, Dir};
use serde::Deserialize;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

/// Builtin ast-grep rules, embedded at compile time for the same reason the
/// builtin skills are: a single binary shouldn't need a side-car data
/// directory installed next to it to have any deterministic coverage at all.
static BUILTIN_RULES: Dir = include_dir!("$CARGO_MANIFEST_DIR/rules-builtin");

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

/// Builds a `ruleId -> category` lookup by parsing every embedded rule file
/// for just its `id`/`category` fields. Rules that fail to parse (shouldn't
/// happen for our own builtin files, but this must never be the reason a
/// whole scan fails) are silently skipped — their findings fall back to the
/// default category via `unwrap_or`.
fn rule_categories() -> HashMap<String, String> {
    let mut map = HashMap::new();
    walk_rule_files(&BUILTIN_RULES, &mut |meta| {
        map.insert(meta.id.clone(), meta.category.clone());
    });
    map
}

/// The set of builtin rule ids declaring `semantic: true` — see
/// `RuleMeta::semantic`'s docs for what that means and why.
pub fn semantic_rule_ids() -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    walk_rule_files(&BUILTIN_RULES, &mut |meta| {
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
pub fn rule_metadata() -> HashMap<String, RuleMetadataBlock> {
    let mut map = HashMap::new();
    walk_rule_files(&BUILTIN_RULES, &mut |meta| {
        if let Some(metadata) = &meta.metadata {
            map.insert(meta.id.clone(), metadata.clone());
        }
    });
    map
}

/// Shared recursive walk over every embedded `.yml`/`.yaml` rule file,
/// parsing each into `RuleMeta` and handing it to `visit` — the one place
/// all of `rule_categories`/`semantic_rule_ids`/`rule_metadata` (and
/// `extract_pattern_rules`'s filtering) read the embedded tree from.
fn walk_rule_files(dir: &Dir, visit: &mut impl FnMut(&RuleMeta)) {
    for file in dir.files() {
        let is_yaml = file.path().extension().is_some_and(|ext| ext == "yml" || ext == "yaml");
        if !is_yaml {
            continue;
        }
        if let Some(contents) = file.contents_utf8() {
            if let Some(meta) = parse_rule_meta(contents) {
                visit(&meta);
            }
        }
    }
    for subdir in dir.dirs() {
        walk_rule_files(subdir, visit);
    }
}

fn metadata_to_meta_map(metadata: &RuleMetadataBlock) -> Option<HashMap<String, serde_json::Value>> {
    match serde_json::to_value(metadata).ok()? {
        serde_json::Value::Object(map) => Some(map.into_iter().collect()),
        _ => None,
    }
}

pub(crate) const SOURCE_EXTENSIONS: &[&str] = &["go", "java", "kt", "kts"];

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

/// Runs the embedded ast-grep rule pack against the given changed files and
/// normalizes matches into analyzer findings. Returns an empty list (not an
/// error) if none of the changed files match a language we ship rules for,
/// or if the `ast-grep` binary isn't on PATH — Stage 1 is meant to degrade
/// gracefully, not block the rest of the review.
pub fn run_ast_grep(repo_root: &Path, changed_files: &[String]) -> anyhow::Result<Vec<AgentFinding>> {
    // A deleted file's path still shows up in `git diff --numstat` — ast-grep
    // tolerates a missing path (skips it with a stderr warning, still scans
    // the rest), but filtering up front avoids the noise and the wasted work.
    let relevant: Vec<&str> = changed_files.iter().map(String::as_str).filter(|p| is_relevant_source_file(p) && repo_root.join(p).exists()).collect();
    if relevant.is_empty() {
        return Ok(vec![]);
    }

    let temp_dir = tempfile::tempdir()?;
    let rules_dir = temp_dir.path().join("rules");
    std::fs::create_dir_all(&rules_dir)?;
    extract_pattern_rules(&BUILTIN_RULES, &rules_dir)?;

    // sgconfig.yml must live outside `rules_dir` — ruleDirs scans every YAML
    // file in that directory recursively, so a config file placed inside it
    // gets misinterpreted as a rule file too.
    let sgconfig_path = temp_dir.path().join("sgconfig.yml");
    std::fs::write(&sgconfig_path, "ruleDirs:\n  - rules\n")?;

    let output = match Command::new("ast-grep").arg("scan").arg("--config").arg(&sgconfig_path).arg("--json").args(&relevant).current_dir(repo_root).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(err.into()),
    };

    // ast-grep exits non-zero when error-severity rules match matches — that's
    // signal, not failure. Only a genuinely unparsable stdout means the tool
    // itself broke (bad rule file, crash, etc).
    let stdout = String::from_utf8_lossy(&output.stdout);
    let matches: Vec<serde_json::Value> = match serde_json::from_str(&stdout) {
        Ok(matches) => matches,
        Err(err) => {
            anyhow::bail!("ast-grep produced unparsable output: {err}. stderr: {}", String::from_utf8_lossy(&output.stderr));
        }
    };

    let categories = rule_categories();
    let metadata = rule_metadata();
    Ok(matches.iter().filter_map(|m| match_to_finding(m, &categories, &metadata)).collect())
}

/// Copies only `kind: pattern` (or `kind`-absent) rule files from `dir`
/// into `dest`, preserving their relative paths. ast-grep's CLI errors if
/// any file under its `ruleDirs` lacks a valid `rule:` key, so a
/// `kind: taint`/`kind: threshold` file (which has no `rule:` block at
/// all — its body is `sources:`/`sinks:`/`metric:`/`threshold:` instead)
/// must never reach the subprocess. This is what makes "one rules
/// directory, three execution backends" possible: every rule lives in the
/// same `rules-builtin/<lang>/<category>/<id>.yml` tree, but only the
/// pattern-kind ones are ever visible to `ast-grep scan`.
fn extract_pattern_rules(dir: &Dir, dest: &Path) -> anyhow::Result<()> {
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
        let dest_path = dest.join(file.path());
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest_path, contents)?;
    }
    for subdir in dir.dirs() {
        extract_pattern_rules(subdir, dest)?;
    }
    Ok(())
}

fn match_to_finding(m: &serde_json::Value, categories: &HashMap<String, String>, metadata: &HashMap<String, RuleMetadataBlock>) -> Option<AgentFinding> {
    let rule_id = m.get("ruleId")?.as_str()?.to_string();
    let file = m.get("file")?.as_str()?.to_string();
    let message = m.get("message").and_then(|v| v.as_str()).unwrap_or("(no message provided by rule)").to_string();
    let severity = map_severity(m.get("severity").and_then(|v| v.as_str()).unwrap_or("warning"));
    let snippet = m.get("lines").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let category = categories.get(&rule_id).cloned().unwrap_or_else(default_category);
    let meta = metadata.get(&rule_id).and_then(metadata_to_meta_map);

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
        let result = run_ast_grep(Path::new("/nonexistent"), &["README.md".to_string(), "package.json".to_string()]).unwrap();
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
        let result = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string(), "deleted.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();

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
    fn finds_a_gob_decode_of_untrusted_input_in_go() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[(
            "main.go",
            "package main\n\nimport \"encoding/gob\"\n\nfunc handle(body []byte) {\n\tvar v MyType\n\tgob.NewDecoder(body).Decode(&v)\n}\n",
        )]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-unreachable-code")));
    }

    #[test]
    fn finds_a_named_empty_interface_in_go() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\ntype Marker interface {\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-empty-interface")));
    }

    #[test]
    fn finds_a_go_test_with_no_assertions() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main_test.go", "package main\n\nimport \"testing\"\n\nfunc TestNothing(t *testing.T) {\n\tx := doSomething()\n\t_ = x\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main_test.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main_test.go".to_string()]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-test-without-assertions")));
    }

    #[test]
    fn does_not_flag_a_non_test_function_named_test_something() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("main.go", "package main\n\nfunc TestingHelperNotATest() int {\n\treturn 1\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("go-test-without-assertions")));
    }

    #[test]
    fn finds_unreachable_code_after_a_throw_in_java() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.java", "public class Foo {\n    void g() {\n        throw new RuntimeException(\"boom\");\n        System.out.println(\"dead\");\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.java".to_string()]).unwrap();
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("java-unreachable-code")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_java_code_after_a_return_inside_an_if_block() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.java", "public class Foo {\n    int k(int x) {\n        if (x > 0) {\n            return x;\n        }\n        return 0;\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.java".to_string()]).unwrap();
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("java-unreachable-code")));
    }

    #[test]
    fn finds_unreachable_code_after_exitprocess_in_kotlin() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.kt", "class Foo {\n    fun h(x: Int) {\n        exitProcess(1)\n        println(\"dead2\")\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.kt".to_string()]).unwrap();
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("kotlin-unreachable-code")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_kotlin_code_after_a_return_inside_an_if_block() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Foo.kt", "class Foo {\n    fun k(x: Int): Int {\n        if (x > 0) {\n            return x\n        }\n        return 0\n    }\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Foo.kt".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["FooTest.java".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["FooTest.java".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["FooTest.kt".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["FooTest.kt".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
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
        let findings = run_ast_grep(dir.path(), &["Sample.java".to_string()]).unwrap();
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("java-empty-catch-block")), "got: {findings:#?}");
    }

    #[test]
    fn finds_a_real_bug_in_kotlin() {
        if !ast_grep_available() {
            eprintln!("skipping: ast-grep not on PATH");
            return;
        }
        let dir = write_repo(&[("Sample.kt", "fun main() {\n    val s: String? = null\n    println(s!!.length)\n}\n")]);
        let findings = run_ast_grep(dir.path(), &["Sample.kt".to_string()]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("kotlin-avoid-not-null-assertion"));
    }

    #[test]
    fn title_from_rule_id_is_human_readable() {
        assert_eq!(title_from_rule_id("go-no-self-comparison"), "Go No Self Comparison");
    }

    #[test]
    fn rule_categories_reads_the_declared_category_for_every_builtin_rule() {
        let categories = rule_categories();
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
        let metadata = rule_metadata();
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
        let findings = run_ast_grep(dir.path(), &["main.go".to_string()]).unwrap();
        let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some("go-weak-hash")).expect("go-weak-hash should fire");
        let meta = finding.meta.as_ref().expect("go-weak-hash declares a metadata: block, meta should be Some");
        assert_eq!(meta.get("cwe").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|v| v.as_str()), Some("CWE-327"));
    }

    #[test]
    fn semantic_rule_ids_reads_rules_declaring_semantic_true() {
        let ids = semantic_rule_ids();
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
        let finding = match_to_finding(&m, &categories, &metadata).unwrap();
        assert_eq!(finding.category, "correctness");
    }
}
