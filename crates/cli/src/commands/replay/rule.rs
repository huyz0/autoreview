use std::path::Path;
use std::process::Command;
use std::collections::BTreeSet;

use anyhow::ensure;
use autoreview_schema::AgentFinding;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Tool {
    AstGrep,
    AutoreviewArchgraph,
    AutoreviewComplexity,
    AutoreviewDataflow,
    AutoreviewDuplication,
    AutoreviewFrameworks,
    AutoreviewRust,
}

#[derive(Debug)]
pub(super) struct IncompleteAnalysis(pub(super) String);

impl std::fmt::Display for IncompleteAnalysis {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "dataflow analysis was incomplete: {}", self.0)
    }
}

impl std::error::Error for IncompleteAnalysis {}

impl Tool {
    pub(super) fn scan(self, source: &Path, paths: &[String], rules: &BTreeSet<String>) -> anyhow::Result<Vec<AgentFinding>> {
        let findings = match self {
            Self::AstGrep => {
                ensure!(
                    Command::new("ast-grep")
                        .arg("--version")
                        .output()
                        .is_ok_and(|output| output.status.success()),
                    "ast-grep is required for this rule"
                );
                autoreview_core::run_ast_grep(source, paths, &[])?
            }
            Self::AutoreviewComplexity => autoreview_core::run_complexity_check(source, paths, &[]),
            Self::AutoreviewArchgraph => autoreview_core::run_archgraph_check(source, paths),
            Self::AutoreviewDataflow => {
                let result = autoreview_core::run_dataflow_check_with_coverage(source, source, paths, &[]);
                let relevant: Vec<_> = result.coverage.iter().filter(|issue| affects_rules(issue, rules)).collect();
                if !relevant.is_empty() {
                    return Err(IncompleteAnalysis(relevant.iter().map(|issue| format!("{}: {}", issue.path.as_deref().unwrap_or("repository"), issue.detail)).collect::<Vec<_>>().join("; ")).into());
                }
                result.findings
            }
            Self::AutoreviewDuplication => autoreview_core::run_duplication_check(source, paths),
            Self::AutoreviewFrameworks => autoreview_core::run_framework_checks(source, paths),
            Self::AutoreviewRust => autoreview_core::run_rust_review(source, paths),
        };
        Ok(findings)
    }
}

fn affects_rules(issue: &autoreview_schema::AnalysisCoverageIssue, rules: &BTreeSet<String>) -> bool {
    if let Some(rule) = &issue.rule {
        return rules.contains(rule);
    }
    issue.analyzer != "call-sequence" || rules.iter().any(|id|
        autoreview_core::find_rule_definition(id, &[]).is_some_and(|rule| rule.kind == "call-sequence"))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rule {
    pub(super) tool: Tool,
    pub(super) id: String,
}

impl Rule {
    pub(super) fn matches(&self, finding: &AgentFinding) -> bool {
        finding.source.rule_id.as_deref() == Some(&self.id)
    }

    pub(super) fn compares_revisions(&self) -> bool {
        self.tool == Tool::AutoreviewFrameworks && self.id == "java-new-nullable-return-breaks-caller"
    }

    pub(super) fn check_replay_support(&self) -> anyhow::Result<()> {
        let definition = autoreview_core::find_rule_definition(&self.id, &[]);
        let kind = definition.as_ref().map(|rule| rule.kind.as_str());
        let supported = match self.tool {
            Tool::AstGrep => kind == Some("pattern"),
            Tool::AutoreviewArchgraph => {
                matches!(self.id.as_str(), "import-cycle" | "web-import-cycle")
            }
            Tool::AutoreviewComplexity => kind == Some("threshold"),
            Tool::AutoreviewDataflow => {
                matches!(kind, Some("taint" | "call-sequence"))
                    || matches!(
                        self.id.as_str(),
                        "typed-nil-interface-return"
                            | "append-shared-backing-array"
                            | "npe-risk-from-helper-return"
                            | "loopvar-capture-pre-1.22"
                            | "loopvar-address-pre-1.22"
                    )
            }
            Tool::AutoreviewDuplication => self.id == "duplicate-code-block",
            Tool::AutoreviewFrameworks => kind == Some("framework"),
            Tool::AutoreviewRust => autoreview_core::rust_review_rule(&self.id).is_some(),
        };
        ensure!(
            supported,
            "rule '{}' is unavailable for replay with {:?}",
            self.id,
            self.tool
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataflow_replay_rejects_incomplete_analysis() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.go"), "package main\nfunc main( {\n").unwrap();
        let error = Tool::AutoreviewDataflow.scan(root.path(), &["main.go".into()], &BTreeSet::from(["go-os-rename-discarded-error".into()])).unwrap_err();
        assert!(error.to_string().contains("dataflow analysis was incomplete"));
    }

    #[test]
    fn dataflow_replay_rejects_unreadable_go_package_sources() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.go"), "package main\nfunc main() {}\n").unwrap();
        std::fs::write(root.path().join("broken.go"), [0xff, 0xfe]).unwrap();
        let error = Tool::AutoreviewDataflow.scan(root.path(), &["main.go".into()], &BTreeSet::from(["go-os-rename-discarded-error".into()])).unwrap_err();
        assert!(error.to_string().contains("broken.go"), "{error}");
        assert!(error.to_string().contains("dataflow analysis was incomplete"), "{error}");
    }

    #[test]
    fn dataflow_replay_rejects_unresolved_local_web_helpers() {
        let root = tempfile::tempdir().unwrap();
        for (path, rule) in [
            ("main.js", "javascript-node-fs-discarded-promise"),
            ("main.ts", "typescript-node-fs-discarded-promise"),
            ("main.tsx", "typescript-node-fs-discarded-promise"),
        ] {
            std::fs::write(root.path().join(path), "import { run } from './missing';\nexport function main() { run(); }\n").unwrap();
            let error = Tool::AutoreviewDataflow.scan(root.path(), &[path.into()], &BTreeSet::from([rule.into()])).unwrap_err();
            assert!(error.to_string().contains("./missing"), "{path}: {error}");
            assert!(error.to_string().contains("dataflow analysis was incomplete"), "{path}: {error}");
        }
    }

    #[test]
    fn dataflow_replay_rejects_ambiguous_barrel_helpers() {
        let root = tempfile::tempdir().unwrap();
        let helper = "import { rename } from 'node:fs/promises';\nexport function move(a, b) { return rename(a, b); }\n";
        for path in ["a.js", "b.js"] {
            std::fs::write(root.path().join(path), helper).unwrap();
        }
        std::fs::write(root.path().join("index.js"), "export * from './a'; export * from './b';\n").unwrap();
        std::fs::write(root.path().join("main.js"), "import { move } from './index'; function run(a, b) { move(a, b); }\n").unwrap();
        let error = Tool::AutoreviewDataflow.scan(root.path(), &["main.js".into()], &BTreeSet::from(["javascript-node-fs-discarded-promise".into()])).unwrap_err();
        assert!(error.to_string().contains("ambiguous"), "{error}");
    }

    #[test]
    fn dataflow_replay_rejects_unresolved_rust_module_alias() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "use crate::missing as helpers;\nfn run() { helpers::discard(std::fs::remove_file(\"file\")); }\n").unwrap();
        let error = Tool::AutoreviewDataflow.scan(root.path(), &["src/lib.rs".into()],
            &BTreeSet::from(["rust-discarded-remove-file-result".into()])).unwrap_err();
        assert!(error.to_string().contains("Rust module import"), "{error}");
        assert!(error.to_string().contains("incomplete"), "{error}");
    }

    #[test]
    fn rule_specific_limitations_do_not_block_unrelated_rules() {
        let issue = autoreview_schema::AnalysisCoverageIssue {
            analyzer: "call-sequence".into(), reason: "path_budget".into(), scope: "function".into(),
            path: Some("main.go".into()), line: Some(1), rule: Some("rule-a".into()), detail: "skipped".into(),
        };
        assert!(affects_rules(&issue, &BTreeSet::from(["rule-a".into()])));
        assert!(!affects_rules(&issue, &BTreeSet::from(["rule-b".into()])));
    }
}
