use super::{make_finding, AgentFinding, Cfg, Node, Severity, Stmt};
use autoreview_dataflow::rules::{go_append_shared_backing_array, go_loopvar};

pub(super) fn run_append_shared_backing_array(
    path: &str,
    lowered: &[(Node, Cfg<Stmt>)],
) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for (_, cfg) in lowered {
        for hit in go_append_shared_backing_array::check(cfg) {
            findings.push(make_finding(
                "append-shared-backing-array",
                "correctness",
                Severity::Medium,
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

pub(super) fn run_loopvar_checks(path: &str, lowered: &[(Node, Cfg<Stmt>)]) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for (_, cfg) in lowered {
        for hit in go_loopvar::check_capture(cfg) {
            let kind = if hit.kind == autoreview_dataflow::cfg::ClosureKind::Goroutine {
                "goroutine"
            } else {
                "deferred closure"
            };
            findings.push(make_finding(
                "loopvar-capture-pre-1.22",
                "correctness",
                Severity::Medium,
                path,
                hit.source_line,
                format!("Loop variable `{}` captured by a {kind} on a pre-1.22 Go module", hit.var),
                format!(
                    "This {kind} references the enclosing loop's `{}` without shadowing it first, and `go.mod` targets a Go version before 1.22. Before 1.22, `for`/`range` loop variables have per-loop (not per-iteration) scope — every {kind} launched by this loop ends up seeing the same, final value of `{}` instead of its own iteration's value. Shadow it first (`{} := {}`) right inside the closure, or pass it as a parameter.",
                    hit.var, hit.var, hit.var, hit.var
                ),
            ));
        }

        for hit in go_loopvar::check_address(cfg) {
            findings.push(make_finding(
                "loopvar-address-pre-1.22",
                "correctness",
                Severity::Medium,
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
