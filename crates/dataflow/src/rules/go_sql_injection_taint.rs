//! `go-sql-injection-taint` — complements the existing
//! `go-sql-string-concat` ast-grep rule, which only catches a query
//! built via literal-string concatenation *in the same call expression*
//! (`db.Query("..." + userInput)`). This rule catches the equally common
//! indirect case the pattern rule structurally can't see: the query
//! built into a variable first (`q := "..." + userInput`), or a tainted
//! value passed straight through with no concatenation at all
//! (`db.Query(userInput)`).
//!
//! No specific Semgrep registry rule ID was found for Go SQL injection
//! during this session's research (noted as a research gap, not a
//! confirmed absence) — this rule is written from first principles
//! (CWE-89) rather than ported from a cited source, unlike
//! `go-command-injection-taint`.

use crate::taint::{NamePattern, TaintSink, TaintSpec};

pub fn spec() -> TaintSpec {
    TaintSpec {
        rule_id: "go-sql-injection-taint",
        sources: vec![NamePattern("FormValue"), NamePattern("PostFormValue")],
        sinks: vec![
            TaintSink { call: NamePattern("Query"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("QueryContext"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("QueryRow"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("QueryRowContext"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("Exec"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("ExecContext"), tainted_arg_positions: None },
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
    fn flags_a_form_value_passed_directly_to_query() {
        let cfg = lower("package p\nfunc handle(r *http.Request, db *sql.DB) {\n\tid := r.FormValue(\"id\")\n\trows, err := db.Query(id)\n\t_ = rows\n\t_ = err\n}\n");
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].sink_call, "db.Query");
    }

    #[test]
    fn flags_a_concatenated_query_built_from_a_tainted_value() {
        let cfg = lower(
            "package p\nfunc handle(r *http.Request, db *sql.DB) {\n\tid := r.FormValue(\"id\")\n\tquery := \"SELECT * FROM users WHERE id=\" + id\n\trows, err := db.Query(query)\n\t_ = rows\n\t_ = err\n}\n",
        );
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?} — indirect concatenation should still be caught");
    }

    #[test]
    fn does_not_flag_a_query_row_call_with_no_tainted_argument() {
        let cfg = lower("package p\nfunc handle(db *sql.DB) {\n\trow := db.QueryRow(\"SELECT * FROM users WHERE id=?\", 42)\n\t_ = row\n}\n");
        assert!(check(&spec(), &cfg).is_empty());
    }
}
