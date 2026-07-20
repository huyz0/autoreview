//! Detects Unicode bidirectional control characters in source text — the
//! "Trojan Source" attack class (CVE-2021-42574): these characters reorder
//! how a line *displays* without changing how it *compiles/parses*, so a
//! reviewer reading the rendered code sees something different from what
//! the compiler sees (e.g. a `// comment` that visually looks like it ends
//! before code that's actually still inside it). Ported from golangci-lint's
//! `bidichk`. Deliberately a plain byte/char scan, not an ast-grep rule or
//! a per-language check — these characters are dangerous wherever they
//! appear in a source file (including inside string literals and comments,
//! which is the whole point of the attack), so scoping this to specific AST
//! node kinds would miss the cases that matter most. Applies uniformly
//! across every changed file regardless of language.

use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

/// The nine bidi control characters golangci-lint's `bidichk` flags:
/// embedding/override (U+202A-U+202E) and isolate (U+2066-U+2069) controls.
/// Directional *marks* (U+200E LRM, U+200F RLM) are deliberately excluded —
/// they don't reorder surrounding text the way these do, so including them
/// would flag legitimate RTL-language comments/strings.
const DANGEROUS_BIDI_CHARS: &[char] = &['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}'];

fn char_name(c: char) -> &'static str {
    match c {
        '\u{202A}' => "LEFT-TO-RIGHT EMBEDDING",
        '\u{202B}' => "RIGHT-TO-LEFT EMBEDDING",
        '\u{202C}' => "POP DIRECTIONAL FORMATTING",
        '\u{202D}' => "LEFT-TO-RIGHT OVERRIDE",
        '\u{202E}' => "RIGHT-TO-LEFT OVERRIDE",
        '\u{2066}' => "LEFT-TO-RIGHT ISOLATE",
        '\u{2067}' => "RIGHT-TO-LEFT ISOLATE",
        '\u{2068}' => "FIRST STRONG ISOLATE",
        '\u{2069}' => "POP DIRECTIONAL ISOLATE",
        _ => "unknown bidi control character",
    }
}

fn make_finding(path: &str, line: u32, c: char) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-practices".to_string(), rule_id: Some("bidi-control-character".to_string()), aspect: None, backend: None },
        category: "security".to_string(),
        severity: Severity::High,
        confidence: 1.0,
        title: "Unicode bidirectional control character in source".to_string(),
        message: format!(
            "This line contains U+{:04X} ({}), a Unicode bidirectional control character. These characters reorder how surrounding text *displays* without changing how it *parses* — the \"Trojan Source\" attack class (CVE-2021-42574): a reviewer reading the rendered code can see something different from what actually compiles/runs, e.g. code hidden inside what looks like a comment or string. Legitimate source code essentially never needs one of these; remove it unless this file is deliberately testing Unicode/bidi handling.",
            c as u32,
            char_name(c)
        ),
        location: Location { path: path.to_string(), range: LocationRange { start_line: line, end_line: Some(line), ..Default::default() }, snippet: String::new(), side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// Scans every changed file's current on-disk content for dangerous bidi
/// control characters, one finding per occurrence. Binary/unreadable files
/// are silently skipped (nothing to scan).
pub fn run_bidi_control_character_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    changed_files
        .iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(repo_root.join(path)).ok()?;
            let findings: Vec<AgentFinding> = content
                .lines()
                .enumerate()
                .flat_map(|(idx, line)| line.chars().filter(|c| DANGEROUS_BIDI_CHARS.contains(c)).map(move |c| make_finding(path, (idx + 1) as u32, c)))
                .collect();
            Some(findings)
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_a_right_to_left_override_character() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f() {\n\t// admin\u{202E} rekcah //\n}\n").unwrap();
        let findings = run_bidi_control_character_check(dir.path(), &["main.go".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("bidi-control-character"));
        assert_eq!(findings[0].location.range.start_line, 4);
    }

    #[test]
    fn does_not_flag_plain_ascii_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f() {\n\tprintln(\"hello\")\n}\n").unwrap();
        let findings = run_bidi_control_character_check(dir.path(), &["main.go".to_string()]);
        assert!(findings.is_empty(), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_left_to_right_or_right_to_left_mark() {
        // U+200E/U+200F are directional *marks*, not embedding/override/
        // isolate controls — they don't reorder surrounding text the way
        // the flagged set does, so a comment using one for a legitimate
        // RTL-language annotation shouldn't be treated as an attack.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\n// \u{200E}note\u{200F}\nfunc f() {}\n").unwrap();
        let findings = run_bidi_control_character_check(dir.path(), &["main.go".to_string()]);
        assert!(findings.is_empty(), "got: {findings:#?}");
    }

    #[test]
    fn flags_every_dangerous_character_in_the_flagged_set() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = DANGEROUS_BIDI_CHARS.iter().map(|c| format!("// {c}\n")).collect();
        std::fs::write(dir.path().join("main.go"), &content).unwrap();
        let findings = run_bidi_control_character_check(dir.path(), &["main.go".to_string()]);
        assert_eq!(findings.len(), DANGEROUS_BIDI_CHARS.len(), "got: {findings:#?}");
    }

    #[test]
    fn works_across_any_file_extension_not_just_go() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Main.java"), "class Main {\n    // admin\u{202E} rekcah //\n}\n").unwrap();
        let findings = run_bidi_control_character_check(dir.path(), &["Main.java".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
    }
}
