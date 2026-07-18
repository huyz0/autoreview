//! Track 4 (code practices) of the rule-pack expansion plan. Deliberately
//! the smallest track — Go practices are already substantially covered by
//! the existing `golangci-lint` wrapper (revive/staticcheck/govet), and
//! Java/Kotlin practices are better served by wrapping real linters
//! (detekt/ktlint/checkstyle) through the generic SARIF ingest adapter
//! (`analyzers::sarif`) than by re-implementing their rules here — that's
//! per-tool wiring work, tracked separately, not done in this module.
//!
//! Implemented as plain text/regex scanning (no ast-grep, no new parse
//! dependency) since none of these three checks are structural in the way
//! ast-grep patterns are suited for — they're about comment *content* and
//! import *text*, not code shape.
//!
//! Scope note: "debug print statements left in non-test code" was in the
//! plan's original sketch and is deliberately dropped here. Distinguishing
//! a genuine debug leftover from legitimate stdout usage (a CLI's own
//! output, a logger shim, deliberate diagnostics) needs context this kind
//! of scanning can't reliably provide — a bare "flag every println" rule
//! would be low-precision noise, which the project's own deterministic-
//! first philosophy explicitly argues against generating.

use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PracticesLanguage {
    Go,
    JavaOrKotlin,
}

fn language_for_file(path: &str) -> Option<PracticesLanguage> {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("go") => Some(PracticesLanguage::Go),
        Some("java") | Some("kt") | Some("kts") => Some(PracticesLanguage::JavaOrKotlin),
        _ => None,
    }
}

fn line_comment_prefix(language: PracticesLanguage) -> &'static str {
    match language {
        PracticesLanguage::Go | PracticesLanguage::JavaOrKotlin => "//",
    }
}

fn make_finding(rule_id: &str, path: &str, start_line: u32, end_line: u32, title: String, message: String, snippet: String) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-practices".to_string(), rule_id: Some(rule_id.to_string()), aspect: None, backend: None },
        category: "style".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title,
        message,
        location: Location { path: path.to_string(), range: LocationRange { start_line, end_line: Some(end_line), ..Default::default() }, snippet, side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// A commented-out code line is one that, stripped of its `//` prefix,
/// still ends in a statement terminator — a strong, low-false-positive
/// signal (real prose comments essentially never end every line with a
/// semicolon) that deliberately trades recall for precision.
fn looks_like_commented_out_code(comment_text: &str) -> bool {
    comment_text.trim_end().ends_with(';')
}

/// Flags runs of 3+ consecutive `//` comment lines that all look like
/// commented-out code, per `looks_like_commented_out_code`.
fn detect_commented_out_code(path: &str, content: &str, language: PracticesLanguage) -> Vec<AgentFinding> {
    let prefix = line_comment_prefix(language);
    let mut findings = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut run_len = 0usize;

    let lines: Vec<&str> = content.lines().collect();
    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        let is_code_like_comment = trimmed.strip_prefix(prefix).map(looks_like_commented_out_code).unwrap_or(false);

        if is_code_like_comment {
            if run_start.is_none() {
                run_start = Some(idx);
            }
            run_len += 1;
        } else {
            if run_len >= 3 {
                let start = run_start.unwrap();
                findings.push(make_finding(
                    "commented-out-code",
                    path,
                    (start + 1) as u32,
                    idx as u32,
                    format!("Commented-out code block ({run_len} lines)"),
                    "A run of consecutive commented-out statements was left in the file — delete it (version control already has the history) or add a comment explaining why it's kept.".to_string(),
                    lines[start..idx].join("\n"),
                ));
            }
            run_start = None;
            run_len = 0;
        }
    }
    if run_len >= 3 {
        let start = run_start.unwrap();
        findings.push(make_finding(
            "commented-out-code",
            path,
            (start + 1) as u32,
            lines.len() as u32,
            format!("Commented-out code block ({run_len} lines)"),
            "A run of consecutive commented-out statements was left in the file — delete it (version control already has the history) or add a comment explaining why it's kept.".to_string(),
            lines[start..].join("\n"),
        ));
    }

    findings
}

/// A TODO/FIXME with no ticket-like reference on the same line (`#123`,
/// `JIRA-123`, `ABC-4567`) is easy to lose track of forever — this doesn't
/// require a *specific* tracker, just some reference a reader could follow.
fn has_ticket_reference(text: &str) -> bool {
    let bytes = text.as_bytes();
    // #123
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '#' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    // ABC-123 (letters, hyphen, digits — Jira-style)
    let word_chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < word_chars.len() {
        if word_chars[i].is_ascii_uppercase() {
            let start = i;
            while i < word_chars.len() && word_chars[i].is_ascii_uppercase() {
                i += 1;
            }
            if i > start && i < word_chars.len() && word_chars[i] == '-' {
                let mut j = i + 1;
                let digit_start = j;
                while j < word_chars.len() && word_chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j > digit_start {
                    return true;
                }
            }
        } else {
            i += 1;
        }
    }
    false
}

fn detect_todo_without_ticket(path: &str, content: &str, language: PracticesLanguage) -> Vec<AgentFinding> {
    let prefix = line_comment_prefix(language);
    let mut findings = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim();
        let Some(comment) = trimmed.strip_prefix(prefix) else { continue };
        let upper = comment.to_uppercase();
        if !(upper.contains("TODO") || upper.contains("FIXME")) {
            continue;
        }
        if has_ticket_reference(comment) {
            continue;
        }
        findings.push(make_finding(
            "todo-without-ticket",
            path,
            (idx + 1) as u32,
            (idx + 1) as u32,
            "TODO/FIXME with no ticket reference".to_string(),
            "This TODO/FIXME has no ticket/issue reference (e.g. #123, JIRA-456) — bare TODOs are easy to lose track of forever. Link it to a tracked issue, or resolve it now.".to_string(),
            trimmed.to_string(),
        ));
    }

    findings
}

/// Fowler's "Comments" smell, the padding variant: a comment block sitting
/// directly above a method that's wildly disproportionate to the method
/// body it documents. Distinguishing a genuinely *stale* comment (one that
/// no longer matches the code) needs semantic understanding this line-scan
/// can't provide, so this only catches the syntactic proxy the session's
/// prior pass flagged as tractable — comment volume vastly exceeding the
/// code volume is itself a maintenance smell (it's either padding that
/// should be trimmed or a sign the method needs splitting up, not more
/// prose). Reuses `complexity::opens_function`'s same brace-style heuristic
/// so both scanners treat function boundaries identically.
fn detect_padding_comment(path: &str, content: &str, language: PracticesLanguage) -> Vec<AgentFinding> {
    use super::complexity::opens_function;

    const MIN_COMMENT_LINES: usize = 10;
    const RATIO: usize = 3;

    let prefix = line_comment_prefix(language);
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();

    let mut idx = 0usize;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if trimmed.strip_prefix(prefix).is_some() {
            let comment_start = idx;
            let mut j = idx;
            while j < lines.len() && lines[j].trim().strip_prefix(prefix).is_some() {
                j += 1;
            }
            let comment_len = j - comment_start;

            if j < lines.len() && opens_function(lines[j].trim()).is_some() && comment_len >= MIN_COMMENT_LINES {
                // Measure the function body by brace depth from its opening line.
                let mut depth = 0i32;
                let mut body_end = j;
                for (k, l) in lines.iter().enumerate().skip(j) {
                    depth += l.matches('{').count() as i32;
                    depth -= l.matches('}').count() as i32;
                    if depth == 0 {
                        body_end = k;
                        break;
                    }
                }
                let body_len = body_end - j + 1;

                if comment_len > RATIO * body_len {
                    findings.push(make_finding(
                        "excessive-comment-padding",
                        path,
                        (comment_start + 1) as u32,
                        j as u32,
                        format!("Comment block ({comment_len} lines) is disproportionate to the method it documents ({body_len} lines)"),
                        "This comment block is far longer than the method it precedes, which often means it's stale/padding rather than useful documentation, or a sign the method itself has grown too complex to explain briefly. Trim it to what's still accurate, or split the method.".to_string(),
                        lines[comment_start..j].join("\n"),
                    ));
                }
            }
            idx = j.max(idx + 1);
        } else {
            idx += 1;
        }
    }

    findings
}

/// A near-universal check across every tool researched for the rule-pack
/// gap analysis (Checkstyle UnusedImports, detekt UnusedImport, Sonar
/// S1128) that this project had zero coverage of. Single-file syntactic:
/// an import's simple name (the last `.`-segment) must appear as a whole
/// word somewhere else in the file, or it's unused. Deliberately skips
/// wildcard imports (already covered by `detect_wildcard_imports`, and
/// there's no single "simple name" to search for), static/Kotlin
/// extension-function imports with an `as` alias (search for the alias
/// instead — same reasoning, just a different name to look for), and
/// annotation-only imports used purely for their side effect on
/// `@Target`-adjacent processors are not special-cased since those still
/// use their simple name at the annotation site (`@MyAnnotation`), so the
/// generic whole-word search already covers them correctly.
fn detect_unused_import(path: &str, content: &str) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        let Some(rest) = trimmed.strip_prefix("import ") else { continue };
        let rest = rest.trim_end_matches(';').trim();
        if rest.ends_with(".*") || rest.starts_with("static ") {
            continue;
        }

        let simple_name = if let Some((_, alias)) = rest.rsplit_once(" as ") {
            alias.trim()
        } else {
            rest.rsplit('.').next().unwrap_or(rest).trim()
        };
        if simple_name.is_empty() {
            continue;
        }

        let used_elsewhere = lines.iter().enumerate().any(|(other_idx, other_line)| {
            if other_idx == idx {
                return false;
            }
            contains_whole_word(other_line, simple_name)
        });

        if !used_elsewhere {
            findings.push(make_finding(
                "unused-import",
                path,
                (idx + 1) as u32,
                (idx + 1) as u32,
                format!("Unused import ({simple_name})"),
                format!("`{simple_name}` is imported but never referenced anywhere else in this file — dead code that adds noise to the file's real dependency list."),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

/// Whole-word substring search — a plain `contains` would also match
/// `simple_name` as a substring of a longer identifier (e.g. `Foo` inside
/// `FooBar`), which isn't a real usage.
fn contains_whole_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let word_bytes = word.as_bytes();
    if word_bytes.is_empty() {
        return false;
    }
    let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(word) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_word_char(bytes[abs - 1]);
        let after_idx = abs + word_bytes.len();
        let after_ok = after_idx >= bytes.len() || !is_word_char(bytes[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
        if start >= haystack.len() {
            break;
        }
    }
    false
}

/// A `private` field/method is only reachable from within its own file's
/// class — unlike `unused-import`, this doesn't need symindex's cross-file
/// index at all, single-file line-scanning is the *correct* scope, not
/// just a cheaper approximation. Real false-positive risk this one carries
/// that unused-import doesn't: reflection-based frameworks (Jackson/Gson
/// serialization, JPA entities, JUnit `@Test`-annotated private helpers
/// invoked by test runners) reference private members by name only in
/// configuration/annotations elsewhere, sometimes not in this file's text
/// at all — marked `semantic: true`-equivalent in `diff.rs`'s hardcoded
/// union for exactly that reason.
///
/// Deliberately simple field-declaration parsing (one declarator per line,
/// no comma-separated multi-name declarations) — same restraint as this
/// file's existing wildcard-import scan.
fn detect_unused_private_field(path: &str, content: &str) -> Vec<AgentFinding> {
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();

    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        if !contains_whole_word(trimmed, "private") || trimmed.ends_with('{') || trimmed.ends_with('}') {
            continue;
        }

        if trimmed.contains('(') {
            continue; // a method declaration, not a field — handled separately
        }

        let name: Option<&str> = if let Some((_, after_keyword)) = trimmed.split_once("val ").or_else(|| trimmed.split_once("var ")) {
            // Kotlin: `private val NAME: TYPE = ...` / `private var NAME = ...`
            Some(after_keyword.split(&[':', '='][..]).next().unwrap_or(after_keyword).trim())
        } else if trimmed.ends_with(';') {
            // Java: `private [modifiers] TYPE NAME [= ...];`
            let body = trimmed.trim_end_matches(';');
            let head = body.split('=').next().unwrap_or(body).trim();
            head.split_whitespace().next_back()
        } else {
            None
        };

        let Some(name) = name.filter(|n| !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_')) else { continue };

        let used_elsewhere = lines.iter().enumerate().any(|(other_idx, other_line)| other_idx != idx && contains_whole_word(other_line, name));
        if !used_elsewhere {
            findings.push(make_finding(
                "unused-private-field",
                path,
                (idx + 1) as u32,
                (idx + 1) as u32,
                format!("Unused private field ({name})"),
                format!("`{name}` is declared `private` but never referenced anywhere else in this file — dead state, or a sign this field was meant to be used and isn't yet. Heuristic: reflection-based frameworks (serialization, JPA, some test runners) can reference private members without a textual match, so this is flagged for a semantic check rather than reported outright."),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

/// Same reasoning as `detect_unused_private_field`, for methods. Doesn't
/// exclude the method's own body from the "used elsewhere" search, so a
/// private method that only calls itself recursively and is otherwise
/// unused won't be flagged — a false negative, the conservative direction.
fn detect_unused_private_method(path: &str, content: &str) -> Vec<AgentFinding> {
    use super::complexity::{function_name, opens_function};

    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();

    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        if !contains_whole_word(trimmed, "private") || opens_function(trimmed).is_none() {
            continue;
        }
        let Some(name) = function_name(trimmed) else { continue };
        // Constructors share their name with the class — a class always
        // "uses" its own constructor implicitly (instantiation happens
        // elsewhere, often in another file), so skip name-matches that
        // look like a capitalized type name to avoid flagging every
        // private constructor as unused.
        if name.chars().next().is_some_and(|c| c.is_uppercase()) {
            continue;
        }

        let used_elsewhere = lines.iter().enumerate().any(|(other_idx, other_line)| other_idx != idx && contains_whole_word(other_line, name));
        if !used_elsewhere {
            findings.push(make_finding(
                "unused-private-method",
                path,
                (idx + 1) as u32,
                (idx + 1) as u32,
                format!("Unused private method ({name})"),
                format!("`{name}` is declared `private` but never called anywhere else in this file — dead code. Heuristic: reflection-based frameworks (some test runners, serialization callbacks) can invoke private methods without a textual match, so this is flagged for a semantic check rather than reported outright."),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

/// Extracts every double-quoted string literal's inner text from one line
/// — a plain char-scanner (not regex) so escaped quotes (`\"`) inside a
/// literal don't prematurely end it. Doesn't distinguish a real string
/// literal from a quoted character inside a line comment (`// say "hi"`),
/// which would double-count occurrences appearing only in comments — a
/// narrow, low-impact over-count, not a false-positive-inducing one (it
/// only makes an already-duplicated literal look *more* duplicated).
fn extract_string_literals(line: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            let mut j = i + 1;
            let mut buf = String::new();
            let mut closed = false;
            while j < chars.len() {
                if chars[j] == '\\' && j + 1 < chars.len() {
                    buf.push(chars[j]);
                    buf.push(chars[j + 1]);
                    j += 2;
                    continue;
                }
                if chars[j] == '"' {
                    closed = true;
                    break;
                }
                buf.push(chars[j]);
                j += 1;
            }
            if closed {
                literals.push(buf);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    literals
}

/// Checkstyle MultipleStringLiterals / detekt StringLiteralDuplication /
/// Sonar S1192 — 3-tool agreement, and this project had no coverage of it
/// at all. The same literal repeated across a file should be a named
/// constant; a typo in one copy silently diverges from the others.
/// Skips short literals (empty string, single-character strings like
/// separators/format specifiers) — too common to be meaningful, matching
/// this project's existing precision-over-recall bias for length-gated
/// checks elsewhere in this file. Marked semantic (hardcoded union in
/// `diff.rs`) since `extract_string_literals` can't distinguish a real
/// literal from quoted text inside a comment, which can inflate the count.
fn detect_duplicate_string_literals(path: &str, content: &str) -> Vec<AgentFinding> {
    const MIN_LITERAL_LEN: usize = 4;
    const MIN_OCCURRENCES: usize = 3;

    let mut occurrences: std::collections::BTreeMap<String, Vec<u32>> = std::collections::BTreeMap::new();
    for (idx, raw_line) in content.lines().enumerate() {
        for literal in extract_string_literals(raw_line) {
            if literal.chars().count() < MIN_LITERAL_LEN {
                continue;
            }
            occurrences.entry(literal).or_default().push((idx + 1) as u32);
        }
    }

    let mut findings = Vec::new();
    for (literal, lines) in occurrences {
        if lines.len() < MIN_OCCURRENCES {
            continue;
        }
        let first_line = lines[0];
        let other_lines = lines[1..].iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", ");
        findings.push(make_finding(
            "duplicate-string-literal",
            path,
            first_line,
            first_line,
            format!("Duplicated string literal ({} occurrences)", lines.len()),
            format!("The string literal \"{literal}\" appears {} times in this file (also at line(s) {other_lines}) — a typo in one copy silently diverges from the others. Extract it to a named constant.", lines.len()),
            format!("\"{literal}\""),
        ));
    }
    findings
}

/// The catch variable's declared name from a `catch (...)` line — Java
/// `catch (Type varname) {`, Kotlin `catch (varname: Type) {`. Returns
/// `None` for anything else (including the already-covered `catch
/// (Exception e) { }` empty-body case, which this function doesn't try to
/// distinguish from a real swallow — that's the caller's job).
fn catch_variable_name(trimmed: &str) -> Option<&str> {
    // The catch clause commonly follows the try/prior-catch block's closing
    // brace on the same line (`} catch (...) {`), not just a bare `catch (`
    // at the start of the line.
    let trimmed = trimmed.trim_start_matches('}').trim_start();
    let rest = trimmed.strip_prefix("catch (").or_else(|| trimmed.strip_prefix("catch("))?;
    let inner = rest.split(')').next()?.trim();
    if let Some((var, _type)) = inner.split_once(':') {
        // Kotlin: `varname: Type`
        Some(var.trim())
    } else {
        // Java: `Type varname` (possibly `Type1 | Type2 varname` for
        // multi-catch — the variable name is still the last token either way)
        inner.split_whitespace().next_back()
    }
}

/// detekt's SwallowedException / Sonar S1166: a catch block that does
/// *something*, but never actually references the exception it caught —
/// broader than `empty-catch-block` (a non-empty body that logs a bare
/// "operation failed" string with no exception detail is just as blind to
/// the real cause as an empty one). Implemented as a hand-rolled brace-scan
/// rather than an ast-grep rule: this needs to check whether the catch
/// variable's *name* (captured dynamically per catch block) appears
/// anywhere in its own body, which requires a metavariable to be
/// substituted into a second match — ast-grep's relational sub-rules
/// (`has`/`not`) don't reliably propagate a captured metavariable's text
/// into a nested pattern/regex in the version this project uses (the same
/// limitation hit and worked around differently while building
/// `rethrow-caught-exception-unchanged` earlier this batch).
fn detect_swallowed_exception(path: &str, content: &str) -> Vec<AgentFinding> {
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();

    let mut idx = 0usize;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if let Some(var_name) = catch_variable_name(trimmed) {
            if trimmed.ends_with('{') {
                // Brace-match the catch body starting after this line — the
                // catch line's own leading `}` (closing the prior try/catch
                // block, when it shares a line like `} catch (...) {`) must
                // not be counted here, or it cancels out against this
                // line's own opening `{` and ends the "body" before it
                // starts. Start counting from depth 1 (this line's `{`
                // already opened the catch body) at the *next* line.
                let mut depth = 1i32;
                let mut body_end = lines.len().saturating_sub(1);
                for (k, l) in lines.iter().enumerate().skip(idx + 1) {
                    depth += l.matches('{').count() as i32;
                    depth -= l.matches('}').count() as i32;
                    if depth == 0 {
                        body_end = k;
                        break;
                    }
                }
                let body_lines = &lines[idx + 1..body_end];
                let body_is_blank = body_lines.iter().all(|l| l.trim().is_empty());
                let var_referenced = body_lines.iter().any(|l| contains_whole_word(l, var_name));

                if !body_is_blank && !var_referenced {
                    findings.push(make_finding(
                        "swallowed-exception",
                        path,
                        (idx + 1) as u32,
                        (body_end + 1) as u32,
                        format!("Swallowed exception ({var_name} never referenced)"),
                        format!("This catch block does something, but never references `{var_name}` — whatever it logs/does has no actual detail about the exception it caught, so debugging the real failure later is just as hard as if the catch block were empty. Reference `{var_name}` (log it, rethrow it, include its message) or make the block genuinely empty with a comment explaining why it's safe to ignore."),
                        trimmed.to_string(),
                    ));
                }
            }
        }
        idx += 1;
    }
    findings
}

fn detect_wildcard_imports(path: &str, content: &str) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim();
        let Some(rest) = trimmed.strip_prefix("import ") else { continue };
        let rest = rest.trim_end_matches(';').trim();
        if rest.ends_with(".*") {
            findings.push(make_finding(
                "wildcard-import",
                path,
                (idx + 1) as u32,
                (idx + 1) as u32,
                "Wildcard import".to_string(),
                "A wildcard import pulls in every symbol from the package, obscuring which specific classes/functions this file actually depends on and risking name collisions as the package grows. Import the specific names used instead.".to_string(),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

const BIDI_CONTROL_CHARS: [char; 9] =
    ['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}'];

/// Flags Unicode bidirectional control characters in source — the "Trojan
/// Source" attack class, where an invisible reorder character makes code
/// display differently than it compiles/runs.
fn detect_bidi_control_character(path: &str, content: &str) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if line.chars().any(|c| BIDI_CONTROL_CHARS.contains(&c)) {
            findings.push(make_finding(
                "bidi-control-character",
                path,
                (idx + 1) as u32,
                (idx + 1) as u32,
                "Unicode bidirectional control character in source".to_string(),
                "This line contains a Unicode bidirectional control character (e.g. U+202E RIGHT-TO-LEFT OVERRIDE). These are invisible in most editors but can make source code display in an order different from how it actually compiles/executes — the \"Trojan Source\" attack class. Remove it unless there's a deliberate, reviewed reason for it (e.g. an RTL string literal, which should use an explicit escape instead).".to_string(),
                line.trim().to_string(),
            ));
        }
    }
    findings
}

/// Flags `for X.Next() { }` iteration loops that never check `X.Err()`
/// afterward — the sql.Rows/similar cursor convention where the loop just
/// stops silently on error instead of surfacing it.
fn detect_iterator_err_not_checked(path: &str, content: &str) -> Vec<AgentFinding> {
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if let Some(rest) = trimmed.strip_prefix("for ") {
            if let Some(dot) = rest.find(".Next()") {
                let recv = rest[..dot].trim();
                let is_identifier = !recv.is_empty() && recv.chars().all(|c| c.is_alphanumeric() || c == '_') && recv.chars().next().is_some_and(|c| !c.is_ascii_digit());
                if is_identifier {
                    let mut depth = 0i32;
                    let mut saw_open = false;
                    let mut end_idx = idx;
                    for (k, l) in lines.iter().enumerate().skip(idx) {
                        depth += l.matches('{').count() as i32;
                        depth -= l.matches('}').count() as i32;
                        if l.contains('{') {
                            saw_open = true;
                        }
                        if saw_open && depth == 0 {
                            end_idx = k;
                            break;
                        }
                    }
                    let needle = format!("{recv}.Err(");
                    let mut found = false;
                    let mut fn_depth = 0i32;
                    for l in lines.iter().skip(end_idx + 1) {
                        if l.contains(&needle) {
                            found = true;
                            break;
                        }
                        fn_depth += l.matches('{').count() as i32;
                        fn_depth -= l.matches('}').count() as i32;
                        if fn_depth < 0 {
                            break;
                        }
                    }
                    if !found {
                        findings.push(make_finding(
                            "iterator-err-not-checked",
                            path,
                            (idx + 1) as u32,
                            (end_idx + 1) as u32,
                            format!("`{recv}.Next()` loop never checks `{recv}.Err()`"),
                            format!("Iterating with `for {recv}.Next() {{ }}` and never checking `{recv}.Err()` afterward hides errors that terminated the loop early (e.g. a dropped connection mid-scan for `sql.Rows`) — the loop just silently stops instead of surfacing the failure. Check `{recv}.Err()` right after the loop."),
                            trimmed.to_string(),
                        ));
                    }
                }
            }
        }
        idx += 1;
    }
    findings
}

/// Extracts VAR from a line shaped like `if (VAR == null) {` (allowing
/// `!= null` negation is deliberately excluded — DCL only ever guards on
/// `== null`). Tolerant of extra whitespace but not other conditions
/// combined via `&&`/`||`, to stay precise about matching only the exact
/// null-check shape DCL needs.
fn double_checked_locking_guard_var(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("if")?.trim_start();
    let rest = rest.strip_prefix('(')?;
    let close = rest.find(')')?;
    let cond = rest[..close].trim();
    let var = cond.strip_suffix("== null")?.trim();
    if var.is_empty() || !var.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
        return None;
    }
    Some(var)
}

/// Flags the classic lazy-singleton double-checked-locking shape (null
/// check, `synchronized` block, re-check, assign) where the guarded field
/// isn't declared `volatile` — without it, JIT/CPU reordering can let
/// another thread observe a non-null reference to a partially-constructed
/// object (the canonical JSR-133 hazard). Java only: Kotlin idiomatically
/// uses `by lazy { }`, which is already synchronized correctly.
fn detect_double_checked_locking_without_volatile(path: &str, content: &str) -> Vec<AgentFinding> {
    if !path.ends_with(".java") {
        return Vec::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();
    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        let Some(var) = double_checked_locking_guard_var(trimmed) else { continue };

        let window = |from: usize, span: usize| lines.iter().skip(from).take(span).map(|l| l.trim());

        let Some(sync_offset) = window(idx + 1, 5).position(|l| l.contains("synchronized")) else { continue };
        let sync_idx = idx + 1 + sync_offset;

        let Some(recheck_offset) = window(sync_idx + 1, 5).position(|l| double_checked_locking_guard_var(l) == Some(var)) else { continue };
        let recheck_idx = sync_idx + 1 + recheck_offset;

        let assign_needle = format!("{var} =");
        let assigns = window(recheck_idx + 1, 5).any(|l| l.starts_with(&assign_needle) && !l.starts_with(&format!("{var} ==")));
        if !assigns {
            continue;
        }

        // Confirm the field is declared elsewhere in the file without `volatile`.
        let field_name = var.rsplit('.').next().unwrap_or(var);
        let is_declared_non_volatile = content.lines().any(|l| {
            let l = l.trim();
            l.ends_with(';')
                && !l.contains("volatile")
                && !l.starts_with("//")
                && (l.contains(&format!(" {field_name};")) || l.contains(&format!(" {field_name} =")))
                && !l.starts_with("if")
                && !l.starts_with("return")
                && !l.starts_with(field_name)
                && !l.contains('(')
        });
        if is_declared_non_volatile {
            findings.push(make_finding(
                "double-checked-locking-no-volatile",
                path,
                (idx + 1) as u32,
                (recheck_idx + 1) as u32,
                format!("Double-checked locking on `{var}` without `volatile`"),
                format!("This looks like the double-checked-locking lazy-init pattern guarding `{var}`, but `{field_name}` isn't declared `volatile`. Without it, JIT/CPU instruction reordering can let another thread observe a non-null reference to a partially-constructed object through this field (the canonical JSR-133 double-checked-locking hazard). Add `volatile` to `{field_name}`'s declaration, or replace this pattern with a simpler holder-class/`enum` singleton."),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

/// Extracts the iterated collection expression from a Java for-each header
/// (`for (Type item : collection) {`), or `None` if the line isn't that
/// shape or the collection expression isn't a simple identifier/field
/// access (keeps the check precise — no attempt to handle method-call
/// collection expressions like `for (X x : getItems())`, since there's no
/// single receiver to check mutation calls against).
fn foreach_collection(trimmed: &str) -> Option<&str> {
    let before_brace = trimmed.strip_suffix('{')?.trim_end();
    let rest = before_brace.strip_prefix("for")?.trim_start();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    let colon_pos = inner.rfind(':')?;
    let collection = inner[colon_pos + 1..].trim();
    let is_simple = !collection.is_empty() && collection.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    is_simple.then_some(collection)
}

/// Flags `collection.add()/remove()/clear()` called on the same collection
/// a Java for-each loop is iterating — throws `ConcurrentModificationException`
/// at runtime (the collection's `modCount` is checked on every `Iterator.next()`).
/// Skips collections whose declaration looks like a concurrent-safe type
/// (`CopyOnWriteArrayList`, `ConcurrentHashMap`, a `Blocking*` queue) since
/// mutating those during iteration is fine by design.
fn detect_concurrent_modification_during_foreach(path: &str, content: &str) -> Vec<AgentFinding> {
    if !path.ends_with(".java") {
        return Vec::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();
    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        let Some(collection) = foreach_collection(trimmed) else { continue };

        let is_concurrent_safe = content
            .lines()
            .any(|l| l.contains(collection) && (l.contains("CopyOnWrite") || l.contains("Concurrent") || l.contains("Blocking")));
        if is_concurrent_safe {
            continue;
        }

        let mut depth = 1i32;
        let mut mutation_line: Option<usize> = None;
        for (k, l) in lines.iter().enumerate().skip(idx + 1) {
            depth += l.matches('{').count() as i32;
            depth -= l.matches('}').count() as i32;
            if depth <= 0 {
                break;
            }
            for suffix in [".add(", ".remove(", ".clear("] {
                if l.contains(&format!("{collection}{suffix}")) {
                    mutation_line = Some(k);
                    break;
                }
            }
            if mutation_line.is_some() {
                break;
            }
        }

        if let Some(mut_idx) = mutation_line {
            findings.push(make_finding(
                "concurrent-modification-during-foreach",
                path,
                (idx + 1) as u32,
                (mut_idx + 1) as u32,
                format!("`{collection}` structurally modified during its own for-each iteration"),
                format!("This loop calls a structural mutation method on `{collection}` (`add`/`remove`/`clear`) while iterating it directly — `ArrayList`/`HashMap`-family collections track a modification count and throw `ConcurrentModificationException` from the next `Iterator.next()` call after a structural change. Use `Iterator.remove()` (via an explicit iterator) instead, collect items to remove/add into a separate collection and apply them after the loop, or use a `CopyOnWriteArrayList`/`ConcurrentHashMap` if concurrent-safe iteration is genuinely needed."),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

/// Reads `go.mod`'s `go` directive and reports whether the module targets
/// a pre-1.22 Go version — before 1.22, `for`/`range` loop variables have
/// per-loop (not per-iteration) scope, so a closure capturing one by
/// reference sees every iteration's final value instead of its own.
/// Defaults to `false` (don't flag) when `go.mod` is missing or
/// unparseable, since a false "pre-1.22" assumption would misfire on
/// current code — precision over recall.
fn go_module_targets_pre_1_22(repo_root: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(repo_root.join("go.mod")) else { return false };
    for line in content.lines() {
        let Some(version) = line.trim().strip_prefix("go ") else { continue };
        let mut parts = version.trim().split('.');
        let Some(major) = parts.next().and_then(|p| p.parse::<u32>().ok()) else { return false };
        let minor_digits: String = parts.next().unwrap_or("").chars().take_while(|c| c.is_ascii_digit()).collect();
        let minor = minor_digits.parse::<u32>().unwrap_or(0);
        return major < 1 || (major == 1 && minor < 22);
    }
    false
}

fn range_loop_vars(trimmed: &str) -> Vec<String> {
    let Some(before_brace) = trimmed.strip_suffix('{') else { return Vec::new() };
    let before_brace = before_brace.trim_end();
    let Some(rest) = before_brace.strip_prefix("for") else { return Vec::new() };
    let rest = rest.trim_start();
    let Some(assign_pos) = rest.find(":=") else { return Vec::new() };
    let (vars_part, after) = rest.split_at(assign_pos);
    if !after[2..].trim_start().starts_with("range") {
        return Vec::new();
    }
    vars_part
        .split(',')
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "_" && v.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .collect()
}

/// Whole-word search for `var` inside `text` (avoids matching `i` inside
/// `id` or `item`).
fn contains_identifier(text: &str, var: &str) -> bool {
    let bytes = text.as_bytes();
    let vlen = var.len();
    let mut start = 0;
    while let Some(pos) = text[start..].find(var) {
        let abs = start + pos;
        let before_ok = abs == 0 || !(bytes[abs - 1] as char).is_alphanumeric() && bytes[abs - 1] != b'_';
        let after_idx = abs + vlen;
        let after_ok = after_idx >= bytes.len() || (!(bytes[after_idx] as char).is_alphanumeric() && bytes[after_idx] != b'_');
        if before_ok && after_ok {
            return true;
        }
        start = abs + vlen;
        if start >= text.len() {
            break;
        }
    }
    false
}

/// Flags `go func() { }` / `defer func() { }` closures (no parameters,
/// i.e. capturing by reference rather than being passed the value) that
/// reference an enclosing `for ... := range ...` loop's variable, on Go
/// modules targeting a pre-1.22 Go version. This is the documented
/// Let's Encrypt-class bug: pre-1.22, all iterations share one variable,
/// so every closure ends up seeing the final value.
fn detect_loopvar_capture(path: &str, content: &str) -> Vec<AgentFinding> {
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();
    let mut depth: i32 = 0;
    // (start_depth, vars) for currently open range loops.
    let mut loop_stack: Vec<(i32, Vec<String>)> = Vec::new();

    let mut idx = 0;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();

        let vars = range_loop_vars(trimmed);
        if !vars.is_empty() {
            loop_stack.push((depth, vars));
            depth += 1;
            idx += 1;
            continue;
        }

        let opens_bare_closure = trimmed.ends_with("go func() {") || trimmed.ends_with("defer func() {") || trimmed == "go func() {" || trimmed == "defer func() {";
        if opens_bare_closure && !loop_stack.is_empty() {
            let active_vars: Vec<&String> = loop_stack.iter().flat_map(|(_, vs)| vs.iter()).collect();

            let mut closure_depth = 1i32;
            let mut end_idx = idx;
            let mut shadowed: Vec<&str> = Vec::new();
            let mut found_var: Option<&str> = None;
            for (k, l) in lines.iter().enumerate().skip(idx + 1) {
                let lt = l.trim();
                // A same-named local shadow copy (`v := v`) right at the
                // top of the closure is the standard pre-1.22 fix — treat
                // that variable as safe for the rest of this closure.
                for v in &active_vars {
                    if lt == format!("{v} := {v}") {
                        shadowed.push(v.as_str());
                    }
                }
                if found_var.is_none() {
                    for v in &active_vars {
                        if !shadowed.contains(&v.as_str()) && contains_identifier(lt, v) {
                            found_var = Some(v.as_str());
                        }
                    }
                }
                closure_depth += l.matches('{').count() as i32;
                closure_depth -= l.matches('}').count() as i32;
                if closure_depth <= 0 {
                    end_idx = k;
                    break;
                }
            }
            if let Some(var) = found_var {
                let kind = if trimmed.contains("go func") { "goroutine" } else { "deferred closure" };
                findings.push(make_finding(
                    "loopvar-capture-pre-1.22",
                    path,
                    (idx + 1) as u32,
                    (end_idx + 1) as u32,
                    format!("Loop variable `{var}` captured by a {kind} on a pre-1.22 Go module"),
                    format!("This {kind} references the enclosing loop's `{var}` without shadowing it first, and `go.mod` targets a Go version before 1.22. Before 1.22, `for`/`range` loop variables have per-loop (not per-iteration) scope — every {kind} launched by this loop ends up seeing the same, final value of `{var}` instead of its own iteration's value (a real, documented class of production bug). Shadow it first (`{var} := {var}`) right inside the closure, or pass it as a parameter."),
                    trimmed.to_string(),
                ));
            }
        }

        idx += 1;
    }
    findings
}

/// Flags `&loopVar` (taking a reference to a `for ... := range` loop
/// variable) directly inside that loop's own body, on Go modules
/// targeting a pre-1.22 Go version — before 1.22, every reference ends
/// up pointing at the same shared variable, so a slice/map of these
/// pointers ends up holding N copies of the final iteration's value
/// instead of each iteration's own value. Subtler than the closure-
/// capture case since it compiles cleanly and often passes casual
/// testing (the bug only shows up once you dereference after the loop).
fn detect_loopvar_address_capture(path: &str, content: &str) -> Vec<AgentFinding> {
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        let vars = range_loop_vars(trimmed);
        if vars.is_empty() {
            idx += 1;
            continue;
        }

        let mut depth = 1i32;
        let mut end_idx = idx;
        let mut shadowed: Vec<&str> = Vec::new();
        let mut found: Option<(&str, usize)> = None;
        for (k, l) in lines.iter().enumerate().skip(idx + 1) {
            let lt = l.trim();
            for v in &vars {
                if lt == format!("{v} := {v}") {
                    shadowed.push(v.as_str());
                }
            }
            if found.is_none() {
                for v in &vars {
                    if !shadowed.contains(&v.as_str()) && contains_identifier(lt, &format!("&{v}")) {
                        found = Some((v.as_str(), k));
                    }
                }
            }
            depth += l.matches('{').count() as i32;
            depth -= l.matches('}').count() as i32;
            if depth <= 0 {
                end_idx = k;
                break;
            }
        }

        if let Some((var, ref_idx)) = found {
            findings.push(make_finding(
                "loopvar-address-pre-1.22",
                path,
                (idx + 1) as u32,
                (ref_idx + 1) as u32,
                format!("Address of loop variable `{var}` taken on a pre-1.22 Go module"),
                format!("This takes `&{var}` inside the loop that declares `{var}`, and `go.mod` targets a Go version before 1.22. Before 1.22, `for`/`range` loop variables have per-loop (not per-iteration) scope — every `&{var}` taken across iterations points at the same shared variable, so a slice/map built from these pointers ends up holding N copies of the loop's final value instead of each iteration's own value. This compiles cleanly and often passes casual testing since the bug only shows up when the collected pointers are dereferenced later. Shadow the variable first (`{var} := {var}`) before taking its address, or upgrade the module's Go version."),
                lines[ref_idx].trim().to_string(),
            ));
        }

        idx = end_idx + 1;
    }
    findings
}

fn go_func_declares_error_return(trimmed: &str) -> bool {
    if !trimmed.starts_with("func ") || !trimmed.ends_with('{') {
        return false;
    }
    let before_brace = trimmed[..trimmed.len() - 1].trim_end();
    let stripped = before_brace.trim_end_matches(')');
    stripped.ends_with("error") && !stripped.ends_with("Error")
}

/// Extracts `(NAME, TYPE)` from a bare `var NAME *TYPE` declaration line
/// (no initializer, so the variable starts out nil).
fn go_nil_pointer_var_decl(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("var ")?;
    if rest.contains('=') {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let name = parts.next()?;
    let ty = parts.next()?;
    ty.starts_with('*').then_some(name)
}

/// Flags a function declared to return `error` (or `(..., error)`) that
/// `return`s a locally-declared `*T` pointer variable directly instead of
/// an `error`-typed value — the classic Go "typed nil in an interface"
/// gotcha: an `error` interface value holding a nil `*T` is itself
/// non-nil (interfaces are a `(type, value)` pair), so `err != nil`
/// checks at the call site pass even though nothing actually went wrong.
fn detect_typed_nil_interface_return(path: &str, content: &str) -> Vec<AgentFinding> {
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if !go_func_declares_error_return(trimmed) {
            idx += 1;
            continue;
        }

        let mut depth = 1i32;
        let mut nil_ptr_vars: Vec<&str> = Vec::new();
        let mut end_idx = idx;
        let mut found: Option<(&str, usize)> = None;
        for (k, l) in lines.iter().enumerate().skip(idx + 1) {
            let lt = l.trim();
            if let Some(name) = go_nil_pointer_var_decl(lt) {
                nil_ptr_vars.push(name);
            }
            if let Some(rest) = lt.strip_prefix("return ") {
                let last = rest.rsplit(',').next().unwrap_or(rest).trim();
                // The standard safe idiom (`if X != nil { return X }`)
                // guards the return with an explicit nil check on the
                // immediately preceding line — don't flag that shape.
                let guarded = k > 0 && lines[k - 1].trim() == format!("if {last} != nil {{");
                if nil_ptr_vars.contains(&last) && !guarded {
                    found = Some((last, k));
                }
            }
            depth += l.matches('{').count() as i32;
            depth -= l.matches('}').count() as i32;
            if depth <= 0 {
                end_idx = k;
                break;
            }
        }

        if let Some((var, ret_idx)) = found {
            findings.push(make_finding(
                "typed-nil-interface-return",
                path,
                (idx + 1) as u32,
                (ret_idx + 1) as u32,
                format!("Returning typed pointer `{var}` where an `error` is expected"),
                format!("This function declares an `error` return, but returns `{var}` — a locally-declared pointer variable — directly instead of an `error`-typed value. An `error` interface value holding a nil `*T` is itself non-nil (interfaces are a `(type, value)` pair internally), so a caller's `if err != nil` check passes even when `{var}` is nil and nothing actually went wrong. Return a literal `nil` when there's no error, or explicitly convert: `if {var} != nil {{ return {var} }}; return nil`."),
                lines[ret_idx].trim().to_string(),
            ));
        }

        idx = end_idx + 1;
    }
    findings
}

/// Runs all Track 4 checks against one changed file's current content.
pub fn run_practices_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    let go_pre_1_22 = go_module_targets_pre_1_22(repo_root);
    changed_files
        .iter()
        .filter_map(|path| {
            let language = language_for_file(path)?;
            let full_path = repo_root.join(path);
            let content = std::fs::read_to_string(&full_path).ok()?;

            let mut findings = detect_commented_out_code(path, &content, language);
            findings.extend(detect_todo_without_ticket(path, &content, language));
            findings.extend(detect_padding_comment(path, &content, language));
            findings.extend(detect_bidi_control_character(path, &content));
            if language == PracticesLanguage::JavaOrKotlin {
                findings.extend(detect_wildcard_imports(path, &content));
                findings.extend(detect_unused_import(path, &content));
                findings.extend(detect_unused_private_field(path, &content));
                findings.extend(detect_unused_private_method(path, &content));
                findings.extend(detect_duplicate_string_literals(path, &content));
                findings.extend(detect_swallowed_exception(path, &content));
                findings.extend(detect_double_checked_locking_without_volatile(path, &content));
                findings.extend(detect_concurrent_modification_during_foreach(path, &content));
            }
            if language == PracticesLanguage::Go {
                findings.extend(detect_iterator_err_not_checked(path, &content));
                findings.extend(detect_typed_nil_interface_return(path, &content));
                if go_pre_1_22 {
                    findings.extend(detect_loopvar_capture(path, &content));
                    findings.extend(detect_loopvar_address_capture(path, &content));
                }
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
    fn flags_a_run_of_three_or_more_commented_out_statements() {
        let content = "func f() {\n\t// x := 1;\n\t// y := 2;\n\t// z := 3;\n\treal := 4\n\t_ = real\n}\n";
        let findings = detect_commented_out_code("main.go", content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("commented-out-code"));
    }

    #[test]
    fn does_not_flag_two_commented_out_statements() {
        let content = "func f() {\n\t// x := 1;\n\t// y := 2;\n\treal := 4\n\t_ = real\n}\n";
        let findings = detect_commented_out_code("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_prose_comments() {
        let content = "func f() {\n\t// This function does something important.\n\t// It has multiple lines of explanation.\n\t// See the docs for more info.\n\treal := 4\n\t_ = real\n}\n";
        let findings = detect_commented_out_code("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_bare_todo() {
        let content = "func f() {\n\t// TODO: fix this properly\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("todo-without-ticket"));
    }

    #[test]
    fn does_not_flag_a_todo_with_a_github_issue_reference() {
        let content = "func f() {\n\t// TODO(#123): fix this properly\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_todo_with_a_jira_style_reference() {
        let content = "func f() {\n\t// TODO(JIRA-456): fix this properly\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_fixme_case_insensitively() {
        let content = "func f() {\n\t// fixme this is broken\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn flags_a_comment_block_wildly_disproportionate_to_its_method() {
        let mut lines = vec!["// line".to_string(); 12];
        lines.push("func f() {".to_string());
        lines.push("\treturn".to_string());
        lines.push("}".to_string());
        let content = lines.join("\n");
        let findings = detect_padding_comment("main.go", &content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("excessive-comment-padding"));
    }

    #[test]
    fn does_not_flag_a_reasonably_sized_doc_comment() {
        let content = "// f does something.\n// It takes no args.\nfunc f() {\n\treturn\n}\n";
        let findings = detect_padding_comment("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_long_comment_over_a_long_method() {
        let mut lines = vec!["// line".to_string(); 12];
        lines.push("func f() {".to_string());
        for _ in 0..40 {
            lines.push("\tdoWork()".to_string());
        }
        lines.push("}".to_string());
        let content = lines.join("\n");
        let findings = detect_padding_comment("main.go", &content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_an_unused_import_in_java() {
        let content = "import java.util.List;\n\npublic class S {\n    int x;\n}\n";
        let findings = detect_unused_import("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("unused-import"));
    }

    #[test]
    fn does_not_flag_a_used_import_in_java() {
        let content = "import java.util.List;\n\npublic class S {\n    List<String> items;\n}\n";
        let findings = detect_unused_import("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_wildcard_import_as_unused() {
        let content = "import java.util.*;\n\npublic class S {}\n";
        let findings = detect_unused_import("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_static_import_as_unused() {
        let content = "import static java.util.Collections.emptyList;\n\npublic class S {}\n";
        let findings = detect_unused_import("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_only_the_simple_name_not_a_substring_match() {
        // "Foo" imported but only "FooBar" appears elsewhere — not a real usage.
        let content = "import com.example.Foo;\n\npublic class S {\n    FooBar x;\n}\n";
        let findings = detect_unused_import("S.java", content);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn checks_the_alias_for_a_kotlin_aliased_import() {
        let content = "import com.example.Foo as Bar\n\nclass S {\n    val x: Bar? = null\n}\n";
        let findings = detect_unused_import("S.kt", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_an_unused_private_field_in_java() {
        let content = "public class S {\n    private int x;\n\n    int getY() { return 1; }\n}\n";
        let findings = detect_unused_private_field("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("unused-private-field"));
    }

    #[test]
    fn does_not_flag_a_used_private_field_in_java() {
        let content = "public class S {\n    private int x;\n\n    int getX() { return x; }\n}\n";
        let findings = detect_unused_private_field("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_public_field_as_unused() {
        let content = "public class S {\n    public int x;\n}\n";
        let findings = detect_unused_private_field("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_an_unused_private_field_in_kotlin() {
        let content = "class S {\n    private val x: Int = 0\n\n    fun getY(): Int = 1\n}\n";
        let findings = detect_unused_private_field("S.kt", content);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn does_not_flag_a_used_private_field_in_kotlin() {
        let content = "class S {\n    private val x: Int = 0\n\n    fun getX(): Int = x\n}\n";
        let findings = detect_unused_private_field("S.kt", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_an_unused_private_method_in_java() {
        let content = "public class S {\n    private void helper() {\n    }\n    public void run() {\n    }\n}\n";
        let findings = detect_unused_private_method("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("unused-private-method"));
    }

    #[test]
    fn does_not_flag_a_called_private_method_in_java() {
        let content = "public class S {\n    private void helper() {\n    }\n    public void run() {\n        helper();\n    }\n}\n";
        let findings = detect_unused_private_method("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_private_constructor_as_an_unused_method() {
        let content = "public class Utils {\n    private Utils() {\n    }\n    public static int f() {\n        return 1;\n    }\n}\n";
        let findings = detect_unused_private_method("Utils.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn extracts_string_literals_from_a_line_handling_escaped_quotes() {
        let literals = extract_string_literals(r#"log("say \"hi\"") + other("second")"#);
        assert_eq!(literals, vec![r#"say \"hi\""#, "second"]);
    }

    #[test]
    fn flags_a_string_literal_repeated_three_or_more_times() {
        let content = "class S {\n    void a() { log(\"connection-refused\"); }\n    void b() { log(\"connection-refused\"); }\n    void c() { log(\"connection-refused\"); }\n}\n";
        let findings = detect_duplicate_string_literals("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("duplicate-string-literal"));
        assert_eq!(findings[0].location.range.start_line, 2);
    }

    #[test]
    fn does_not_flag_a_literal_repeated_only_twice() {
        let content = "class S {\n    void a() { log(\"connection-refused\"); }\n    void b() { log(\"connection-refused\"); }\n}\n";
        let findings = detect_duplicate_string_literals("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_short_literals_even_if_repeated() {
        let content = "class S {\n    void a() { log(\"-\"); }\n    void b() { log(\"-\"); }\n    void c() { log(\"-\"); }\n}\n";
        let findings = detect_duplicate_string_literals("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_catch_block_that_never_references_the_exception() {
        let content = "class S {\n    void a() {\n        try {\n            risky();\n        } catch (IOException e) {\n            log(\"operation failed\");\n        }\n    }\n}\n";
        let findings = detect_swallowed_exception("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("swallowed-exception"));
    }

    #[test]
    fn does_not_flag_a_catch_block_that_references_the_exception() {
        let content = "class S {\n    void a() {\n        try {\n            risky();\n        } catch (IOException e) {\n            log(\"operation failed\", e);\n        }\n    }\n}\n";
        let findings = detect_swallowed_exception("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_an_empty_catch_block_as_swallowed() {
        // Already covered by the separate empty-catch-block ast-grep rule.
        let content = "class S {\n    void a() {\n        try {\n            risky();\n        } catch (IOException e) {\n        }\n    }\n}\n";
        let findings = detect_swallowed_exception("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_kotlin_catch_block_that_never_references_the_exception() {
        let content = "class S {\n    fun a() {\n        try {\n            risky()\n        } catch (e: IOException) {\n            log(\"operation failed\")\n        }\n    }\n}\n";
        let findings = detect_swallowed_exception("S.kt", content);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn flags_a_wildcard_import_in_java() {
        let content = "import java.util.*;\n\npublic class S {}\n";
        let findings = detect_wildcard_imports("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("wildcard-import"));
    }

    #[test]
    fn does_not_flag_a_specific_import_in_java() {
        let content = "import java.util.List;\n\npublic class S {}\n";
        let findings = detect_wildcard_imports("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_wildcard_import_in_kotlin() {
        let content = "import java.util.*\n\nfun f() {}\n";
        let findings = detect_wildcard_imports("S.kt", content);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn run_practices_check_skips_go_files_for_wildcard_imports() {
        // Go has no wildcard import syntax at all — sanity check that the
        // Go path never even calls detect_wildcard_imports.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(1) }\n").unwrap();
        let findings = run_practices_check(dir.path(), &["main.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_practices_check_finds_issues_in_a_real_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("S.java"), "import java.util.*;\n\npublic class S {\n    // TODO fix this\n}\n").unwrap();
        let findings = run_practices_check(dir.path(), &["S.java".to_string()]);
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn run_practices_check_skips_files_that_no_longer_exist() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run_practices_check(dir.path(), &["deleted.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_right_to_left_override_character() {
        let content = "func f() {\n\t// admin\u{202E} \u{202D}rekcah\n}\n";
        let findings = detect_bidi_control_character("main.go", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("bidi-control-character"));
    }

    #[test]
    fn does_not_flag_ordinary_source() {
        let content = "func f() {\n\treturn 1\n}\n";
        let findings = detect_bidi_control_character("main.go", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_double_checked_locking_without_volatile() {
        let content = "public class Singleton {\n    private static Singleton instance;\n\n    public static Singleton getInstance() {\n        if (instance == null) {\n            synchronized (Singleton.class) {\n                if (instance == null) {\n                    instance = new Singleton();\n                }\n            }\n        }\n        return instance;\n    }\n}\n";
        let findings = detect_double_checked_locking_without_volatile("Singleton.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("double-checked-locking-no-volatile"));
    }

    #[test]
    fn does_not_flag_double_checked_locking_with_volatile() {
        let content = "public class Singleton {\n    private static volatile Singleton instance;\n\n    public static Singleton getInstance() {\n        if (instance == null) {\n            synchronized (Singleton.class) {\n                if (instance == null) {\n                    instance = new Singleton();\n                }\n            }\n        }\n        return instance;\n    }\n}\n";
        let findings = detect_double_checked_locking_without_volatile("Singleton.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_list_mutated_during_its_own_foreach() {
        let content = "class Foo {\n    void f(java.util.List<String> items) {\n        for (String item : items) {\n            if (item.isEmpty()) {\n                items.remove(item);\n            }\n        }\n    }\n}\n";
        let findings = detect_concurrent_modification_during_foreach("Foo.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("concurrent-modification-during-foreach"));
    }

    #[test]
    fn does_not_flag_a_copy_on_write_list_mutated_during_foreach() {
        let content = "class Foo {\n    void f() {\n        java.util.List<String> items = new CopyOnWriteArrayList<>();\n        for (String item : items) {\n            items.remove(item);\n        }\n    }\n}\n";
        let findings = detect_concurrent_modification_during_foreach("Foo.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_foreach_that_only_reads() {
        let content = "class Foo {\n    void f(java.util.List<String> items) {\n        for (String item : items) {\n            System.out.println(item);\n        }\n    }\n}\n";
        let findings = detect_concurrent_modification_during_foreach("Foo.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_typed_nil_pointer_returned_as_error() {
        let content = "func do() error {\n\tvar e *myError\n\tif somethingBad {\n\t\te = &myError{}\n\t}\n\treturn e\n}\n";
        let findings = detect_typed_nil_interface_return("main.go", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("typed-nil-interface-return"));
    }

    #[test]
    fn does_not_flag_the_guarded_nil_check_idiom() {
        let content = "func do() error {\n\tvar e *myError\n\tif somethingBad {\n\t\te = &myError{}\n\t}\n\tif e != nil {\n\t\treturn e\n\t}\n\treturn nil\n}\n";
        let findings = detect_typed_nil_interface_return("main.go", content);
        assert!(findings.is_empty(), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_function_that_does_not_return_error() {
        let content = "func do() int {\n\tvar e *myError\n\treturn 0\n}\n";
        let findings = detect_typed_nil_interface_return("main.go", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_goroutine_capturing_the_loop_variable_pre_1_22() {
        let content = "func f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n";
        let findings = detect_loopvar_capture("main.go", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("loopvar-capture-pre-1.22"));
    }

    #[test]
    fn does_not_flag_a_goroutine_that_shadows_the_loop_variable() {
        let content = "func f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\titem := item\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n";
        let findings = detect_loopvar_capture("main.go", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_goroutine_passed_the_variable_as_a_parameter() {
        let content = "func f(items []string) {\n\tfor _, item := range items {\n\t\tgo func(item string) {\n\t\t\tprintln(item)\n\t\t}(item)\n\t}\n}\n";
        let findings = detect_loopvar_capture("main.go", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_the_address_of_a_loop_variable_pre_1_22() {
        let content = "func f(items []string) []*string {\n\tvar out []*string\n\tfor _, item := range items {\n\t\tout = append(out, &item)\n\t}\n\treturn out\n}\n";
        let findings = detect_loopvar_address_capture("main.go", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("loopvar-address-pre-1.22"));
    }

    #[test]
    fn does_not_flag_a_shadowed_address_of_a_loop_variable() {
        let content = "func f(items []string) []*string {\n\tvar out []*string\n\tfor _, item := range items {\n\t\titem := item\n\t\tout = append(out, &item)\n\t}\n\treturn out\n}\n";
        let findings = detect_loopvar_address_capture("main.go", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn go_module_pre_1_22_reads_the_go_directive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\n\ngo 1.20\n").unwrap();
        assert!(go_module_targets_pre_1_22(dir.path()));
    }

    #[test]
    fn go_module_1_22_and_later_is_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\n\ngo 1.23\n").unwrap();
        assert!(!go_module_targets_pre_1_22(dir.path()));
    }

    #[test]
    fn missing_go_mod_defaults_to_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!go_module_targets_pre_1_22(dir.path()));
    }

    #[test]
    fn flags_a_rows_next_loop_that_never_checks_err() {
        let content = "func f(db *sql.DB) {\n\trows, _ := db.Query(\"x\")\n\tdefer rows.Close()\n\tfor rows.Next() {\n\t}\n}\n";
        let findings = detect_iterator_err_not_checked("main.go", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("iterator-err-not-checked"));
    }

    #[test]
    fn does_not_flag_a_rows_next_loop_that_checks_err_afterward() {
        let content = "func f(db *sql.DB) {\n\trows, _ := db.Query(\"x\")\n\tdefer rows.Close()\n\tfor rows.Next() {\n\t}\n\tif err := rows.Err(); err != nil {\n\t\tpanic(err)\n\t}\n}\n";
        let findings = detect_iterator_err_not_checked("main.go", content);
        assert!(findings.is_empty());
    }
}
