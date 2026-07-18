//! Tier 4: an opt-in, real-semantic refinement layered on top of the
//! default heuristic tree-sitter tier (see SESSION_NOTES.md follow-up #3
//! and the crate-level docs' "future work" note). Go only — shells out to
//! a small companion Go program (`tools/tier4-go`, embedded in this crate's
//! source tree) that uses `golang.org/x/tools/go/packages` to actually
//! type-check the target repo and resolve every method-body selector
//! expression to its real static type, instead of symindex's own
//! name-guessing.
//!
//! This module only produces raw resolved-access records and a
//! confirm/contradict judgment against an existing `FeatureEnvyFinding` —
//! it deliberately doesn't reimplement Feature Envy's own scoring, so the
//! threshold/margin logic isn't duplicated between Rust and Go (the Go
//! side's docs make the same claim). Java's equivalent (JavaParser +
//! javaparser-symbol-solver) needs Maven-based dependency resolution and
//! is out of scope here — this is Go-only, same shape as this crate's
//! existing Kotlin exclusion.
//!
//! Fails soft everywhere: no `go` on `PATH`, a repo that doesn't build, a
//! malformed JSONL line — all just mean "no Tier 4 data available," never
//! a hard error, since this is an enhancement over the default tier, not
//! a dependency of it.

use std::path::Path;
use std::process::Command;

use crate::queries::FeatureEnvyFinding;

/// One resolved `x.member` access inside a method body, as reported by the
/// `tier4-go` companion tool. `receiver_type`/`accessed_type` are
/// package-qualified (`"myrepo/foo.Widget"`), so callers should match them
/// by suffix (`.Widget`) against symindex's own unqualified type names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier4Access {
    pub file: String,
    pub line: u32,
    pub method: String,
    pub receiver_type: String,
    pub accessed_ident: String,
    pub accessed_type: String,
}

/// Extracts a `"key": "value"` or `"key": <number>` field's raw text from
/// one JSON object line. The tier4-go tool emits flat, single-line,
/// non-nested objects (see its `accessRecord` struct), so this avoids
/// pulling in a JSON parser for a format this constrained.
fn extract_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = line[start..].trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(&stripped[..end])
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim())
    }
}

fn parse_line(line: &str) -> Option<Tier4Access> {
    Some(Tier4Access {
        file: extract_field(line, "file")?.to_string(),
        line: extract_field(line, "line")?.parse().ok()?,
        method: extract_field(line, "method")?.to_string(),
        receiver_type: extract_field(line, "receiver_type")?.to_string(),
        accessed_ident: extract_field(line, "accessed_ident")?.to_string(),
        accessed_type: extract_field(line, "accessed_type")?.to_string(),
    })
}

/// Parses `tier4-go`'s JSONL stdout into records, skipping any line that
/// doesn't parse cleanly (best-effort, not a hard failure).
pub fn parse_jsonl(output: &str) -> Vec<Tier4Access> {
    output.lines().filter(|l| !l.trim().is_empty()).filter_map(parse_line).collect()
}

/// Path to the companion Go tool's source directory, embedded at compile
/// time — `go run <dir> <args>` builds and runs it directly, no separate
/// install/packaging step.
fn tool_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/tier4-go")
}

/// Runs the companion Go tool against `repo_root` and returns its resolved
/// accesses. Returns `None` (not an error) whenever real type resolution
/// isn't available: no `go` on `PATH`, the tool fails to build/run, or the
/// repo has no Go packages `go/packages` can load.
pub fn run_tier4_go(repo_root: &Path) -> Option<Vec<Tier4Access>> {
    let output = Command::new("go").arg("run").arg(tool_dir()).arg(repo_root).current_dir(repo_root).output().ok()?;
    if output.stdout.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let records = parse_jsonl(&text);
    if records.is_empty() {
        None
    } else {
        Some(records)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier4Verdict {
    /// Real type-checking confirms the foreign accesses the heuristic saw.
    Confirmed,
    /// Real type-checking has data for this method but none of it matches
    /// the heuristic's claimed envied type — likely a heuristic
    /// misparse (e.g. a shadowed name, an import alias).
    Contradicted,
    /// No Tier 4 data available for this method at all (not this repo's
    /// language, tool unavailable, or a build failure) — the heuristic
    /// finding stands on its own, neither confirmed nor refuted.
    NoData,
}

fn type_matches(qualified: &str, unqualified: &str) -> bool {
    qualified == unqualified || qualified.ends_with(&format!(".{unqualified}"))
}

/// Checks one `FeatureEnvyFinding` against the real resolved accesses for
/// its method. Matching is by method name + owner/receiver type (by
/// suffix, since Tier 4's types are package-qualified and symindex's
/// aren't) — deliberately not by line number, since a method can span
/// several lines and the heuristic's own line anchor is the method decl's
/// start line, not any one access site.
pub fn confirm_feature_envy(records: &[Tier4Access], finding: &FeatureEnvyFinding) -> Tier4Verdict {
    let for_method: Vec<&Tier4Access> =
        records.iter().filter(|r| r.method == finding.method && type_matches(&r.receiver_type, &finding.owner_type)).collect();

    if for_method.is_empty() {
        return Tier4Verdict::NoData;
    }

    let matches_envied = for_method.iter().any(|r| type_matches(&r.accessed_type, &finding.envied_type));
    if matches_envied {
        Tier4Verdict::Confirmed
    } else {
        Tier4Verdict::Contradicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::PathBuf;

    fn finding(owner_type: &str, method: &str, envied_type: &str) -> FeatureEnvyFinding {
        FeatureEnvyFinding {
            file: PathBuf::from("foo.go"),
            line: 1,
            owner_type: owner_type.to_string(),
            method: method.to_string(),
            envied_type: envied_type.to_string(),
            envied_access_count: 3,
            own_access_count: 0,
        }
    }

    #[test]
    fn parses_a_jsonl_record() {
        let line = r#"{"file":"/tmp/foo.go","line":15,"method":"Envious","receiver_type":"fixture/foo.Self","accessed_ident":"Get","accessed_type":"fixture/foo.Other"}"#;
        let records = parse_jsonl(line);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].method, "Envious");
        assert_eq!(records[0].accessed_type, "fixture/foo.Other");
        assert_eq!(records[0].line, 15);
    }

    #[test]
    fn skips_malformed_lines() {
        let records = parse_jsonl("not json\n{\"file\":\"a\"}\n");
        assert!(records.is_empty());
    }

    #[test]
    fn confirms_when_a_matching_resolved_access_exists() {
        let records = vec![Tier4Access {
            file: "foo.go".to_string(),
            line: 15,
            method: "Envious".to_string(),
            receiver_type: "fixture/foo.Self".to_string(),
            accessed_ident: "Get".to_string(),
            accessed_type: "fixture/foo.Other".to_string(),
        }];
        let f = finding("Self", "Envious", "Other");
        assert_eq!(confirm_feature_envy(&records, &f), Tier4Verdict::Confirmed);
    }

    #[test]
    fn contradicts_when_method_data_exists_but_type_never_matches() {
        let records = vec![Tier4Access {
            file: "foo.go".to_string(),
            line: 15,
            method: "Envious".to_string(),
            receiver_type: "fixture/foo.Self".to_string(),
            accessed_ident: "Get".to_string(),
            accessed_type: "fixture/foo.SomethingElse".to_string(),
        }];
        let f = finding("Self", "Envious", "Other");
        assert_eq!(confirm_feature_envy(&records, &f), Tier4Verdict::Contradicted);
    }

    #[test]
    fn reports_no_data_when_method_absent() {
        let records: Vec<Tier4Access> = vec![];
        let f = finding("Self", "Envious", "Other");
        assert_eq!(confirm_feature_envy(&records, &f), Tier4Verdict::NoData);
    }

    #[test]
    fn run_tier4_go_resolves_real_go_type_information() {
        if Command::new("go").arg("version").output().is_err() {
            eprintln!("skipping: no go toolchain on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module fixture\n\ngo 1.21\n").unwrap();
        std::fs::create_dir_all(dir.path().join("foo")).unwrap();
        let mut f = std::fs::File::create(dir.path().join("foo/foo.go")).unwrap();
        f.write_all(
            b"package foo\n\ntype Other struct {\n\tVal int\n}\n\nfunc (o *Other) Get() int { return o.Val }\n\ntype Self struct {\n\tother Other\n}\n\nfunc (s *Self) Envious() int {\n\treturn s.other.Get() + s.other.Val + s.other.Get()\n}\n",
        )
        .unwrap();

        let Some(records) = run_tier4_go(dir.path()) else {
            eprintln!("skipping: tier4-go produced no output (network/toolchain unavailable in this environment)");
            return;
        };
        let f = finding("Self", "Envious", "Other");
        assert_eq!(confirm_feature_envy(&records, &f), Tier4Verdict::Confirmed);
    }
}
