//! `go-path-traversal-taint` — an HTTP-form-value reaching a file-system
//! API without going through a path-cleaning/validation step first. No
//! specific Semgrep registry rule ID was found for Go path traversal
//! during this session's research (Semgrep's own documented path-
//! traversal rule found was JAX-RS/Java-specific); this is written from
//! first principles (CWE-22), same as `go-sql-injection-taint`.
//!
//! Deliberately no sanitizer declared: `filepath.Clean` doesn't actually
//! prevent traversal on its own (it normalizes `..` segments but a
//! cleaned `../../etc/passwd` is still a traversal) — declaring it as a
//! sanitizer would create a false sense of safety. A real sanitizer here
//! would need to check the cleaned path stays within a base directory,
//! which is a runtime property this syntactic engine can't verify.

use crate::taint::{NamePattern, TaintSink, TaintSpec};

pub fn spec() -> TaintSpec {
    TaintSpec {
        rule_id: "go-path-traversal-taint",
        sources: vec![NamePattern("FormValue"), NamePattern("PostFormValue")],
        sinks: vec![
            TaintSink { call: NamePattern("Open"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("OpenFile"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("Create"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("ReadFile"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("WriteFile"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("Remove"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("RemoveAll"), tainted_arg_positions: None },
        ],
        sanitizers: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::go::lower_function;
    use crate::taint::check;

    fn lower(source: &str) -> crate::cfg::Cfg<crate::cfg::Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node = root.named_children(&mut cursor).find(|n| n.kind() == "function_declaration").expect("no function_declaration found");
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn flags_a_form_value_used_directly_to_open_a_file() {
        let cfg = lower("package p\nfunc handle(r *http.Request) {\n\tname := r.FormValue(\"file\")\n\tf, err := os.Open(name)\n\t_ = f\n\t_ = err\n}\n");
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].sink_call, "os.Open");
    }

    #[test]
    fn flags_a_form_value_concatenated_into_a_path_before_reading() {
        let cfg = lower("package p\nfunc handle(r *http.Request) {\n\tname := r.FormValue(\"file\")\n\tpath := \"/uploads/\" + name\n\tdata, err := os.ReadFile(path)\n\t_ = data\n\t_ = err\n}\n");
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_a_hardcoded_path() {
        let cfg = lower("package p\nfunc f() {\n\tdata, err := os.ReadFile(\"/etc/config.json\")\n\t_ = data\n\t_ = err\n}\n");
        assert!(check(&spec(), &cfg).is_empty());
    }
}
