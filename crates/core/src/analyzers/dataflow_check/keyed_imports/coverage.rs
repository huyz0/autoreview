use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::AnalysisCoverageIssue;
use autoreview_dataflow::call_order::keyed_flow_exhausted;
use super::{CallOrderRuleDef, LoweredFile, Source};

pub(super) fn record_flow_limits(
    sources: &BTreeMap<String, Source>,
    lowered: &[LoweredFile<'_>],
    rules: &[(&CallOrderRuleDef, Language)],
    coverage: &mut Vec<AnalysisCoverageIssue>,
) {
    for ((path, file), (_, graphs)) in sources.iter().zip(lowered) {
        if !rules.iter().any(|(_, language)| file.language.matches_rule_language(*language)) {
            continue;
        }
        for (node, graph) in graphs {
            if keyed_flow_exhausted(graph) {
                let mut limitation = issue(Some(path), "control_flow_limit", "function", "The finally-path lowering budget was exhausted; call-sequence analysis skipped this function.");
                limitation.line = Some(node.start_position().row as u32 + 1);
                coverage.push(limitation);
            }
        }
    }
}

pub(super) fn record_rule_limits(coverage: &mut Vec<AnalysisCoverageIssue>, rule_id: &str, limitations: Vec<autoreview_dataflow::call_order::KeyedLimitation>) {
    for limitation in limitations {
        let detail = limitation_detail(limitation.reason);
        let mut record = issue(limitation.path.as_deref(), limitation.reason,
            if limitation.path.is_some() { "function" } else { "rule" }, detail);
        record.line = limitation.line.filter(|line| *line > 0);
        record.rule = Some(rule_id.to_owned());
        coverage.push(record);
    }
}

fn limitation_detail(reason: &str) -> &'static str {
    verified_detail(reason).or_else(|| typed_detail(reason)).unwrap_or(match reason {
            "path_budget" => "The path-state budget was exhausted; this rule skipped the affected root.",
            "unmodeled_cleanup_call" => "A possible cleanup call was not represented in the control-flow graph; this rule skipped the affected function.",
            "helper_summary_limit" => "Helper summaries did not converge within the analysis budget; cross-file result tracking was skipped.",
            "unsupported_rust_control_flow" => "Rust control flow in this function could not be lowered; call-sequence analysis was skipped.",
            "rust_match_binding_projection" => "Rust match pattern names are tracked as distinct local identities; their relationship to the matched value and whether they denote constants are not resolved.",
            "unresolved_helper_target" => "The helper target is ambiguous or depends on unavailable exports; cross-file call-sequence analysis may be incomplete.",
            "unresolved_result_receiver" => "A result reached an unresolved helper or an unmodeled helper argument; whether it is used could not be determined.",
            "wrapped_result_receiver" => "A result was passed in an array or spread argument; the receiver's use of that result could not be determined.",
            "reexport_resolution_limit" => "The re-export resolution budget was exhausted; cross-file call-sequence analysis may be incomplete.",
            _ => "The rule could not model a path safely; analysis of the affected root was skipped.",
    })
}

fn verified_detail(reason: &str) -> Option<&'static str> {
    go_detail(reason).or_else(|| Some(match reason {
            "unmodeled_contract_handle" => "A matching resource or task acquisition occurs in a function with unsupported handle storage or transfer; this rule skipped the function.",
            "unmodeled_conditional_result" => "A result-producing call occurs in a conditional expression whose branches could not be modeled; result-use analysis may be incomplete.",
            "verified_source_unmodeled" => "A verified result source is outside modeled function flow; result-use analysis skipped this call.",
            "invalid_result_components" => "Verified result component information was invalid; analysis skipped the affected call.",
            _ => return None,
    }))
}

fn go_detail(reason: &str) -> Option<&'static str> {
    go_probe_detail(reason).or_else(|| Some(match reason {
            "typed_go_error_source_unavailable" => "Go error type verification is not configured; this rule was skipped.",
            "go_multi_result_unmodeled" => "A multi-result Go call with an error occurs in an unsupported context; result-use analysis skipped this call.",
            "go_dependency_evidence_incomplete" => "Requested Go dependency error-origin evidence could not be verified; summaries are incomplete.",
            _ => return None,
    }))
}

fn go_probe_detail(reason: &str) -> Option<&'static str> {
    Some(match reason {
            "go_error_checker_incomplete" => "Go error type verification could not complete for this package; result-use analysis may be incomplete.",
            "go_error_package_limit" => "The Go error checker package limit was reached; remaining packages were not verified.",
            "go_error_call_limit" => "The Go error checker call-site limit was exceeded; this package was not verified.",
            "go_error_package_load_incomplete" => "The Go error checker could not load the complete package and its dependencies; result-use analysis may be incomplete.",
            "go_error_typecheck_incomplete" => "The Go error checker could not parse or type-check the selected sources; result-use analysis may be incomplete.",
            "go_error_source_mismatch" => "The Go error checker sources did not match the reviewed snapshot; result-use analysis was skipped.",
            "go_error_probe_invalid_request" => "The Go error checker rejected the package configuration or request; result-use analysis was skipped.",
            _ => return None,
    })
}

fn typed_detail(reason: &str) -> Option<&'static str> {
    checker_detail(reason).or_else(|| promise_detail(reason))
}

fn checker_detail(reason: &str) -> Option<&'static str> {
    Some(match reason {
            "typescript_checker_incomplete" => "The configured TypeScript checker failed or could not complete; typed promise analysis may be incomplete.",
            "typescript_compiler_api_unsupported" => "The configured TypeScript compiler does not provide the API required by this checker; typed promise analysis was skipped.",
            "typescript_project_type_errors" => "The TypeScript project has type errors; typed promise analysis was skipped.",
            "typescript_config_invalid" => "The TypeScript configuration is invalid; typed promise analysis was skipped.",
            "typescript_config_unreadable" => "The TypeScript configuration could not be read; typed promise analysis was skipped.",
            _ => return None,
    })
}

fn promise_detail(reason: &str) -> Option<&'static str> {
    Some(match reason {
            "typed_promise_source_unavailable" => "Typed promise source verification is not configured; this rule was skipped.",
            "factory_binding_inventory_incomplete" => "The module's bindings could not be inventoried completely; typed promise analysis may be incomplete.",
            "factory_module_graph_incomplete" => "The module graph does not match the parsed sources; typed promise analysis was skipped.",
            "promise_type_unresolved" => "The promise type could not be verified at this call; typed promise analysis may be incomplete.",
            "factory_call_unmodeled" => "An imported call uses optional-call or spread syntax that typed promise analysis cannot model; this call was skipped.",
            "factory_flow_unmodeled" => "An enclosing function contains unsupported control flow and an imported call; typed promise analysis may be incomplete.",
            _ => return None,
    })
}


pub(super) fn scope_coverage(
    coverage: Vec<AnalysisCoverageIssue>, rules: &[(&CallOrderRuleDef, Language)], entries: &BTreeSet<String>,
) -> Vec<AnalysisCoverageIssue> {
    let applicable: Vec<_> = rules.iter().filter(|(_, declared)| {
        entries.iter().any(|path| language_for_path(Path::new(path)).is_some_and(|language| language.matches_rule_language(*declared)))
    }).map(|(rule, _)| rule.id.as_str()).collect();
    coverage.into_iter().flat_map(|issue| {
        if issue.rule.is_some() || issue.reason != "dependency_limit" { return vec![issue]; }
        applicable.iter().map(|rule| {
            let mut scoped = issue.clone();
            scoped.rule = Some((*rule).to_owned());
            scoped
        }).collect()
    }).collect()
}


pub(super) fn issue(path: Option<&str>, reason: &str, scope: &str, detail: &str) -> AnalysisCoverageIssue {
    AnalysisCoverageIssue {
        analyzer: "call-sequence".into(), reason: reason.into(), scope: scope.into(),
        path: path.map(str::to_owned), line: None, rule: None, detail: detail.into(),
    }
}

