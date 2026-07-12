use std::path::Path;
use std::process::Command;

use include_dir::{include_dir, Dir};

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

/// Builtin ast-grep rules, embedded at compile time for the same reason the
/// builtin skills are: a single binary shouldn't need a side-car data
/// directory installed next to it to have any deterministic coverage at all.
static BUILTIN_RULES: Dir = include_dir!("$CARGO_MANIFEST_DIR/rules-builtin");

const SOURCE_EXTENSIONS: &[&str] = &["go", "java", "kt", "kts"];

fn is_relevant_source_file(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).map(|ext| SOURCE_EXTENSIONS.contains(&ext)).unwrap_or(false)
}

fn map_severity(sg_severity: &str) -> Severity {
    match sg_severity {
        "error" => Severity::High,
        "warning" => Severity::Medium,
        "info" => Severity::Low,
        "hint" => Severity::Info,
        _ => Severity::Medium,
    }
}

fn title_from_rule_id(rule_id: &str) -> String {
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
    BUILTIN_RULES.extract(&rules_dir)?;

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

    Ok(matches.iter().filter_map(match_to_finding).collect())
}

fn match_to_finding(m: &serde_json::Value) -> Option<AgentFinding> {
    let rule_id = m.get("ruleId")?.as_str()?.to_string();
    let file = m.get("file")?.as_str()?.to_string();
    let message = m.get("message").and_then(|v| v.as_str()).unwrap_or("(no message provided by rule)").to_string();
    let severity = map_severity(m.get("severity").and_then(|v| v.as_str()).unwrap_or("warning"));
    let snippet = m.get("lines").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let range = m.get("range")?;
    // ast-grep reports 0-indexed line/column; our schema is 1-indexed to
    // match how editors and `git diff` line numbers read.
    let start_line = range.get("start")?.get("line")?.as_u64()? as u32 + 1;
    let start_col = range.get("start")?.get("column").and_then(|v| v.as_u64()).map(|c| c as u32 + 1);
    let end_line = range.get("end")?.get("line")?.as_u64()? as u32 + 1;
    let end_col = range.get("end")?.get("column").and_then(|v| v.as_u64()).map(|c| c as u32 + 1);

    Some(AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".to_string(), rule_id: Some(rule_id.clone()), aspect: None, backend: None },
        // All builtin rules today are narrow correctness patterns; per-rule
        // category metadata (security/style/etc) is a reasonable M2 addition
        // once the rule pack grows beyond that.
        category: "correctness".to_string(),
        severity,
        confidence: 1.0,
        title: title_from_rule_id(&rule_id),
        message,
        location: Location { path: file, range: LocationRange { start_line, start_col, end_line: Some(end_line), end_col }, snippet, side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
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
        assert_eq!(finding.location.path, "main.go");
        assert_eq!(finding.location.range.start_line, 4); // 1-indexed: line 4 is "if true == true {"
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
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("java-empty-catch-block"));
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
}
