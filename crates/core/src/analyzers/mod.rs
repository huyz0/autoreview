pub mod apply_condition;
pub mod architecture;
pub mod archgraph_check;
pub mod ast_grep;
pub mod bidi_check;
pub mod call_order_rules;
pub mod cargo_audit;
pub mod cargo_review;
pub mod churn;
pub mod clippy;
pub mod complexity;
pub mod dataflow_check;
pub mod duplication;
pub mod framework_checks;
pub mod practices;
pub mod rule_pack;
pub mod rust_review;
pub mod shadow_rules;
pub mod golangci_lint;
pub mod sarif;
pub mod symindex_check;
pub mod taint_rules;
pub mod threshold_rules;

fn analyzer_finding(
    (tool, id): (&str, &str),
    category: &str,
    severity: autoreview_schema::Severity,
    (title, message): (String, String),
    location: autoreview_schema::Location,
) -> autoreview_schema::AgentFinding {
    use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind};
    AgentFinding {
        source: FindingSource {
            kind: FindingSourceKind::Analyzer,
            tool: tool.into(),
            rule_id: Some(id.into()),
            aspect: None,
            backend: None,
        },
        category: category.into(),
        severity,
        confidence: 1.0,
        title,
        message,
        location,
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}
