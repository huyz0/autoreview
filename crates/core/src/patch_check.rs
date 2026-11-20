//! Post-patch syntax validation via a real tree-sitter reparse — the piece
//! of the plan's "Patch-suggestion sanity check" section explicitly deferred
//! at M2 time ("Full tree-sitter re-parse validation... is deferred —
//! `git apply --check` already rejects the failure mode that actually
//! matters"). `git apply --check` proves a patch applies cleanly against
//! the *current* tree; it says nothing about whether the *result* is still
//! syntactically valid Go/Java — an LLM-suggested patch can apply cleanly
//! and still produce broken code (a missing brace, an unterminated string).
//! This module answers exactly that second question, narrowly.
//!
//! Language coverage follows `autoreview_langsupport`.

use std::path::Path;

use autoreview_langsupport::{language_for_path, parser_for};

/// Parses `content` as the language implied by `path`'s extension and
/// reports whether the resulting tree is error-free. Returns `None` (not a
/// verdict) when the language isn't supported or the parser itself fails to
/// load — callers should treat `None` as "skip this check", never as
/// "failed".
pub fn parses_cleanly(path: &Path, content: &str) -> Option<bool> {
    let mut parser = parser_for(language_for_path(path)?)?;
    let tree = parser.parse(content, None)?;
    Some(!tree.root_node().has_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn valid_go_parses_cleanly() {
        let content = "package main\n\nfunc main() {\n\tprintln(\"hi\")\n}\n";
        assert_eq!(parses_cleanly(&PathBuf::from("main.go"), content), Some(true));
    }

    #[test]
    fn go_with_a_missing_brace_fails_to_parse_cleanly() {
        let content = "package main\n\nfunc main() {\n\tprintln(\"hi\")\n";
        assert_eq!(parses_cleanly(&PathBuf::from("main.go"), content), Some(false));
    }

    #[test]
    fn valid_java_parses_cleanly() {
        let content = "class Main {\n    static void main(String[] args) {\n        System.out.println(\"hi\");\n    }\n}\n";
        assert_eq!(parses_cleanly(&PathBuf::from("Main.java"), content), Some(true));
    }

    #[test]
    fn java_with_an_unterminated_string_fails_to_parse_cleanly() {
        let content = "class Main {\n    static void main(String[] args) {\n        System.out.println(\"hi);\n    }\n}\n";
        assert_eq!(parses_cleanly(&PathBuf::from("Main.java"), content), Some(false));
    }

    #[test]
    fn valid_kotlin_parses_cleanly() {
        let content = "fun main() {\n    println(\"hi\")\n}\n";
        assert_eq!(parses_cleanly(&PathBuf::from("Main.kt"), content), Some(true));
    }

    #[test]
    fn kotlin_with_a_missing_brace_fails_to_parse_cleanly() {
        let content = "fun main() {\n    println(\"hi\")\n";
        assert_eq!(parses_cleanly(&PathBuf::from("Main.kt"), content), Some(false));
    }

    #[test]
    fn valid_typescript_parses_cleanly() {
        let content = "function main(): void {\n    console.log(\"hi\");\n}\n";
        assert_eq!(parses_cleanly(&PathBuf::from("main.ts"), content), Some(true));
    }

    #[test]
    fn typescript_with_a_missing_brace_fails_to_parse_cleanly() {
        let content = "function main(): void {\n    console.log(\"hi\");\n";
        assert_eq!(parses_cleanly(&PathBuf::from("main.ts"), content), Some(false));
    }

    #[test]
    fn valid_tsx_parses_cleanly() {
        let content = "function App() {\n    return <div>hi</div>;\n}\n";
        assert_eq!(parses_cleanly(&PathBuf::from("App.tsx"), content), Some(true));
    }

    #[test]
    fn valid_javascript_parses_cleanly() {
        let content = "function main() {\n    console.log(\"hi\");\n}\n";
        assert_eq!(parses_cleanly(&PathBuf::from("main.js"), content), Some(true));
    }

    #[test]
    fn javascript_with_a_missing_brace_fails_to_parse_cleanly() {
        let content = "function main() {\n    console.log(\"hi\");\n";
        assert_eq!(parses_cleanly(&PathBuf::from("main.js"), content), Some(false));
    }

    #[test]
    fn unsupported_language_returns_none_not_a_verdict() {
        assert_eq!(parses_cleanly(&PathBuf::from("README.md"), "# broken (("), None);
    }

    #[test]
    fn parses_rust_and_rejects_an_unclosed_function() {
        let path = Path::new("main.rs");
        assert_eq!(parses_cleanly(path, "fn main() { println!(\"hello\"); }"), Some(true));
        assert_eq!(parses_cleanly(path, "fn main() { println!(\"hello\");"), Some(false));
    }

    #[test]
    fn a_file_with_no_extension_returns_none() {
        assert_eq!(parses_cleanly(&PathBuf::from("Makefile"), "all:\n\techo hi\n"), None);
    }
}
