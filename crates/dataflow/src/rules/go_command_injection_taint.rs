//! `go-command-injection-taint` — a real dataflow taint rule, sourced
//! from Semgrep's own Go command-injection cheat sheet
//! (<https://semgrep.dev/docs/cheat-sheets/go-command-injection>), which
//! documents `exec.Command`/`exec.CommandContext` and `syscall.Exec`/
//! `syscall.ForkExec` as the sink family. Sink scope for this first
//! landing deliberately excludes the cheat sheet's other two sinks
//! (`exec.Cmd{}` struct-literal field assignment, and `cmd.StdinPipe()`)
//! — both need multi-value assignment support (`Cmd.Args = tainted`, or
//! `stdin, err := cmd.StdinPipe()`) this crate's lowering doesn't have
//! yet (see `lower::go::lower_assign_like`'s own documented
//! single-target-only scope). Tracked as a follow-up, not silently
//! dropped.
//!
//! Source scope: `http.Request.FormValue`/`PostFormValue` — matched
//! syntactically by trailing method name (see `taint::NamePattern`), not
//! type-resolved, same precision level Semgrep's own pattern-based rules
//! operate at. No sanitizer is declared for this first landing (matches
//! several of Semgrep's own real registry rules for this exact reason —
//! e.g. `spring-csrf-disabled` also ships with none) — a real allowlist/
//! regex-validation sanitizer is a reasonable follow-up once this rule
//! has real-world false-positive data to react to, not something to
//! guess at up front.

use crate::taint::{NamePattern, TaintSink, TaintSpec};

pub fn spec() -> TaintSpec {
    TaintSpec {
        rule_id: "go-command-injection-taint",
        sources: vec![NamePattern("FormValue"), NamePattern("PostFormValue")],
        sinks: vec![
            TaintSink { call: NamePattern("exec.Command"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("exec.CommandContext"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("syscall.Exec"), tainted_arg_positions: None },
            TaintSink { call: NamePattern("syscall.ForkExec"), tainted_arg_positions: None },
        ],
        sanitizers: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::Stmt;
    use crate::lower::go::lower_function;
    use crate::taint::check;
    use tree_sitter::Node;

    fn lower(source: &str) -> crate::cfg::Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node: Node = root.named_children(&mut cursor).find(|n| n.kind() == "function_declaration").expect("no function_declaration found");
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn flags_a_form_value_reaching_exec_command() {
        let cfg = lower("package p\nfunc handle(r *http.Request) {\n\tuserInput := r.FormValue(\"cmd\")\n\texec.Command(\"sh\", \"-c\", userInput)\n}\n");
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].sink_call, "exec.Command");
    }

    #[test]
    fn flags_a_post_form_value_reaching_syscall_exec() {
        let cfg = lower("package p\nfunc handle(r *http.Request) {\n\tbin := r.PostFormValue(\"bin\")\n\tsyscall.Exec(bin, args, env)\n}\n");
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].sink_call, "syscall.Exec");
    }

    #[test]
    fn does_not_flag_a_literal_only_command() {
        let cfg = lower("package p\nfunc f() {\n\texec.Command(\"ls\", \"-la\")\n}\n");
        assert!(check(&spec(), &cfg).is_empty());
    }

    #[test]
    fn does_not_flag_an_untainted_local_variable_reaching_the_sink() {
        let cfg = lower("package p\nfunc f() {\n\tsafe := \"ls\"\n\texec.Command(safe)\n}\n");
        assert!(check(&spec(), &cfg).is_empty(), "got: {:#?}", check(&spec(), &cfg));
    }
}
